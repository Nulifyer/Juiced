mod support;

use serde_json::{Value, json};
use support::{
    ADMIN_KEY, CLIENT_KEY, Server,
    cli::{failure, invoke, success},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

async fn call(server: &Server, args: &[&str]) -> Value {
    success(
        invoke(
            Some(&server.base),
            args,
            &[("LEMONADE_ADMIN_API_KEY", ADMIN_KEY)],
            None,
        )
        .await,
    )
}

async fn controlled_reply(
    status: u16,
    bytes: Vec<u8>,
    declared: Option<usize>,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            socket.read_exact(&mut byte).await.unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192);
        }
        let reason = if status == 200 {
            "OK"
        } else {
            "Internal Server Error"
        };
        let header = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            declared.unwrap_or(bytes.len())
        );
        socket.write_all(header.as_bytes()).await.unwrap();
        socket.write_all(&bytes).await.unwrap();
    });
    (origin, task)
}

#[tokio::test]
async fn actual_cli_reads_status_catalog_models_and_provider_options_with_explicit_environment_auth()
 {
    let server = Server::new(true).await;
    let status = success(
        invoke(
            Some(&server.base),
            &["status"],
            &[
                ("LEMONADE_ADMIN_API_KEY", ADMIN_KEY),
                ("LEMONADE_API_KEY", "invalid-inference-credential"),
            ],
            None,
        )
        .await,
    );
    assert_eq!(status["implementation"], "rust-experiment");
    assert_eq!(status["resources"]["reserved_bytes"], 0);
    assert_eq!(
        call(&server, &["list"]).await["data"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        call(&server, &["list", "--filter", "ONE"]).await["data"][0]["id"],
        "fixture.one"
    );
    assert_eq!(
        call(&server, &["models"]).await["data"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let providers = call(&server, &["backends"]).await;
    assert_eq!(providers["engine_installation"], false);
    assert_eq!(providers["data"].as_array().unwrap().len(), 3);
    let port = reqwest::Url::parse(&server.base)
        .unwrap()
        .port()
        .unwrap()
        .to_string();
    let status = success(
        invoke(
            None,
            &["status"],
            &[
                ("LEMONADE_HOST", "127.0.0.1"),
                ("LEMONADE_PORT", &port),
                ("LEMONADE_API_KEY", CLIENT_KEY),
            ],
            None,
        )
        .await,
    );
    assert_eq!(status["schema_version"], 1);
    let read_only = failure(
        invoke(
            Some(&server.base),
            &["--api-key", CLIENT_KEY, "load", "fixture.one"],
            &[("LEMONADE_ADMIN_API_KEY", ADMIN_KEY)],
            None,
        )
        .await,
        4,
    );
    assert_eq!(read_only["error"]["status"], 401);
    assert_eq!(
        success(
            invoke(
                Some(&server.base),
                &["--api-key", ADMIN_KEY, "load", "fixture.one"],
                &[("LEMONADE_ADMIN_API_KEY", "invalid-admin-credential")],
                None
            )
            .await
        )["loaded"],
        true
    );
    assert_eq!(
        call(&server, &["unload", "fixture.one"]).await["loaded"],
        false
    );
    assert_eq!(call(&server, &["pause"]).await["paused"], true);
    let paused = failure(
        invoke(
            Some(&server.base),
            &["load", "fixture.one"],
            &[("LEMONADE_ADMIN_API_KEY", ADMIN_KEY)],
            None,
        )
        .await,
        5,
    );
    assert_eq!(paused["error"]["code"], "admission_paused");
    assert_eq!(call(&server, &["resume"]).await["paused"], false);
}

#[tokio::test]
async fn actual_cli_options_lifecycle_policy_and_registration_preserve_server_owned_state_across_restart()
 {
    let mut server = Server::configured(true, |config| {
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["max_inflight"] = json!(1);
        model["options"] = json!({"parallel_slots":1,"context_tokens":512});
    })
    .await;
    let before = call(&server, &["options", "fixture.one"]).await;
    let revision = before["revision"].as_u64().unwrap().to_string();
    let requested = json!({"parallel_slots":1,"context_tokens":1024,"threads":3});
    let options = success(
        invoke(
            Some(&server.base),
            &[
                "options",
                "fixture.one",
                "--file",
                "-",
                "--revision",
                &revision,
            ],
            &[("LEMONADE_ADMIN_API_KEY", ADMIN_KEY)],
            Some(&serde_json::to_vec(&requested).unwrap()),
        )
        .await,
    );
    assert_eq!(options["saved"]["context_tokens"], 1024);
    let loaded = call(&server, &["load", "fixture.one"]).await;
    assert_eq!(loaded["loaded"], true);
    let pid = loaded["pid"].clone();
    let context = call(&server, &["context", "fixture.one"]).await;
    assert_eq!(context["applied"]["pid"], pid);
    assert_eq!(context["observed"]["slots"][0]["context_tokens"], 1024);
    let (status, completion) = server
        .post(
            "completions",
            json!({"model":"fixture.one","prompt":"CLI lifecycle pipeline"}),
        )
        .await;
    assert!(status.is_success());
    assert_eq!(
        completion["received_body_sha256"].as_str().unwrap().len(),
        64
    );
    assert_eq!(
        call(&server, &["unload", "fixture.one"]).await["loaded"],
        false
    );
    let revision = options["revision"].as_u64().unwrap().to_string();
    let hot = call(
        &server,
        &["policy", "fixture.one", "hot", "--revision", &revision],
    )
    .await;
    assert_eq!(hot["policy"], "hot");
    assert_eq!(hot["loaded"], false);
    server.restart().await;
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == true)
        .await;
    let revision = hot["revision"].as_u64().unwrap().to_string();
    let demand = call(
        &server,
        &[
            "policy",
            "fixture.one",
            "on-demand",
            "--revision",
            &revision,
        ],
    )
    .await;
    assert_eq!(demand["policy"], "on-demand");
    assert_eq!(
        call(&server, &["unload", "fixture.one"]).await["loaded"],
        false
    );
    let revision = demand["revision"].as_u64().unwrap().to_string();
    let disabled = call(
        &server,
        &["policy", "fixture.one", "disabled", "--revision", &revision],
    )
    .await;
    assert_eq!(disabled["policy"], "disabled");
    server.restart().await;
    assert_eq!(server.model("fixture.one").await["policy"], "disabled");
    assert_eq!(
        call(&server, &["options", "fixture.one"]).await["saved"]["context_tokens"],
        1024
    );
    let mut registration = server.config["models"][1].clone();
    let id = "CLI / model λ";
    registration["id"] = json!(id);
    registration["policy"] = json!("disabled");
    registration["revision"] = json!(0);
    let path = server
        .config_path
        .parent()
        .unwrap()
        .join("CLI registration.json");
    std::fs::write(&path, serde_json::to_vec(&registration).unwrap()).unwrap();
    let registered = call(&server, &["register", "--file", path.to_str().unwrap()]).await;
    assert_eq!(registered["id"], id);
    assert_eq!(registered["policy"], "disabled");
    let revision = registered["revision"].as_u64().unwrap().to_string();
    let removed = call(&server, &["delete", id, "--revision", &revision]).await;
    assert_eq!(removed["registration_removed"], true);
    assert_eq!(removed["weights_retained"], true);
    assert!(std::path::Path::new(registration["model_file"].as_str().unwrap()).is_file());
}

#[tokio::test]
async fn actual_cli_local_input_usage_connection_auth_and_service_errors_have_distinct_exit_codes()
{
    let server = Server::new(true).await;
    let environment = [("LEMONADE_ADMIN_API_KEY", ADMIN_KEY)];
    let missing = server.config_path.parent().unwrap().join("missing.json");
    assert_eq!(
        failure(
            invoke(
                Some(&server.base),
                &["register", "--file", missing.to_str().unwrap()],
                &environment,
                None
            )
            .await,
            1
        )["error"]["code"],
        "input_unavailable"
    );
    for input in [b"{".as_slice(), b"{} {}".as_slice(), b"[]".as_slice()] {
        failure(
            invoke(
                Some(&server.base),
                &["register", "--file", "-"],
                &environment,
                Some(input),
            )
            .await,
            2,
        );
    }
    let oversized = vec![b' '; 2 * 1024 * 1024 + 1];
    failure(
        invoke(
            Some(&server.base),
            &["register", "--file", "-"],
            &environment,
            Some(&oversized),
        )
        .await,
        2,
    );
    failure(
        invoke(
            Some(&server.base),
            &["options", "fixture.one", "--revision", "0", "--file", "-"],
            &environment,
            Some(br#"{"command":"unexpected"}"#),
        )
        .await,
        2,
    );
    for args in [
        vec!["load"],
        vec!["delete", "fixture.one", "--revision", "-1"],
        vec!["policy", "fixture.one", "unknown", "--revision", "0"],
        vec!["status", "--unexpected"],
    ] {
        failure(
            invoke(Some(&server.base), &args, &environment, None).await,
            2,
        );
    }
    let auth = failure(
        invoke(
            Some(&server.base),
            &["status"],
            &[("LEMONADE_API_KEY", "invalid-cli-key")],
            None,
        )
        .await,
        4,
    );
    assert_eq!(auth["error"]["status"], 401);
    let rejected = failure(
        invoke(Some(&server.base), &["load", "missing"], &environment, None).await,
        5,
    );
    assert_eq!(rejected["error"]["status"], 404);
    assert_eq!(rejected["error"]["code"], "unknown_model");
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    assert_eq!(
        failure(
            invoke(Some(&origin), &["status"], &environment, None).await,
            3
        )["error"]["code"],
        "connection_failed"
    );
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(server.get("juiced/requests").await["data"], json!([]));
}

#[tokio::test]
async fn actual_cli_rejects_implicit_credential_targets_and_unsafe_origins_before_discovery() {
    for args in [vec!["status"], vec!["--discovery", "status"]] {
        let error = failure(
            invoke(None, &args, &[("LEMONADE_ADMIN_API_KEY", ADMIN_KEY)], None).await,
            2,
        );
        assert_eq!(error["error"]["code"], "invalid_arguments");
        assert!(
            error["error"]["message"]
                .as_str()
                .unwrap()
                .contains("explicit")
        );
    }
    for origin in [
        "http://user:private-credential@127.0.0.1:1",
        "http://127.0.0.1:1/path",
        "http://127.0.0.1:1/?token=private-credential",
        "http://127.0.0.1:1/#fragment",
        "file:///tmp/cli",
    ] {
        let output = invoke(Some(origin), &["status"], &[], None).await;
        assert!(!String::from_utf8_lossy(&output.stderr).contains("private-credential"));
        failure(output, 2);
    }
}

#[tokio::test]
async fn actual_cli_rejects_incompatible_malformed_and_oversized_response_bodies() {
    for (body, declared, code) in [
        (
            br#"{"schema_version":1,"implementation":"another-service"}"#.as_slice(),
            None,
            "incompatible_service",
        ),
        (b"invalid json".as_slice(), None, "invalid_response"),
        (b"".as_slice(), Some(2 * 1024 * 1024 + 1), "response_limit"),
    ] {
        let (origin, task) = controlled_reply(200, body.to_vec(), declared).await;
        let error = failure(invoke(Some(&origin), &["status"], &[], None).await, 6);
        assert_eq!(error["error"]["code"], code);
        task.await.unwrap();
    }
}

#[tokio::test]
async fn actual_cli_redacts_selected_credentials_from_untrusted_success_and_error_json() {
    let key = "cli-redaction-synthetic-credential";
    for status in [200, 500] {
        let body = if status == 200 {
            json!({"schema_version":1,"implementation":"rust-experiment","echo":key,"nested":{(key):key}})
        } else {
            json!({"error":{"code":format!("code-{key}"),"message":format!("message-{key}")}})
        };
        let (origin, task) =
            controlled_reply(status, serde_json::to_vec(&body).unwrap(), None).await;
        let output = invoke(
            Some(&origin),
            &["status"],
            &[("LEMONADE_ADMIN_API_KEY", key)],
            None,
        )
        .await;
        let value = if status == 200 {
            success(output)
        } else {
            failure(output, 5)
        };
        if status == 200 {
            assert_eq!(value["echo"], "[redacted]");
            assert_eq!(value["nested"]["[redacted]"], "[redacted]");
        } else {
            assert_eq!(value["error"]["code"], "code-[redacted]");
            assert_eq!(value["error"]["message"], "message-[redacted]");
        }
        task.await.unwrap();
    }
}
