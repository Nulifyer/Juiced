mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{Server, huggingface::Hub};

const HUB_TOKEN: &str = "barista-hub-token-never-returned";
const RESULT_BYTES: usize = 12 * 1024;

const SETTINGS: &str = "/v1/juiced/barista";
const CALL: &str = "/v1/juiced/barista/tools/call";

async fn settings(server: &Server) -> Value {
    let (status, value) = server
        .json(Method::GET, SETTINGS, None, server.admin_key())
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

async fn configure(server: &Server, enabled: bool, permission: &str) -> Value {
    let revision = settings(server).await["revision"].clone();
    let (status, value) = server
        .json(
            Method::POST,
            SETTINGS,
            Some(json!({"expected_revision":revision,"enabled":enabled,"model":"fixture.one","permission":permission})),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    value
}

async fn call(
    server: &Server,
    key: Option<&str>,
    revision: &Value,
    name: &str,
    arguments: Value,
    confirmed: bool,
) -> (StatusCode, Value) {
    server
        .json(
            Method::POST,
            CALL,
            Some(json!({"name":name,"arguments":arguments,"revision":revision,"confirmed":confirmed})),
            key,
        )
        .await
}

fn names(view: &Value) -> Vec<String> {
    view["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn barista_starts_off_and_its_settings_require_operator_authority() {
    let server = Server::new(true).await;
    let initial = settings(&server).await;
    assert_eq!(initial["enabled"], false);
    assert_eq!(initial["model"], Value::Null);
    assert_eq!(initial["permission"], "full");
    assert_eq!(initial["role"], "juiced-assistant");
    assert_eq!(initial["qualified"], false);
    assert_eq!(initial["tools"], json!([]));
    for key in [server.read_key(), None] {
        let (status, _) = server.json(Method::GET, SETTINGS, None, key).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = server
            .json(
                Method::POST,
                SETTINGS,
                Some(json!({"expected_revision":0,"enabled":true,"model":"fixture.one","permission":"full"})),
                key,
            )
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = call(
            &server,
            key,
            &json!(0),
            "juiced_list_models",
            json!({}),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    let (status, refused) = call(
        &server,
        server.admin_key(),
        &json!(0),
        "juiced_list_models",
        json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(refused["error"]["code"], "assistant_disabled");

    for (model, code) in [
        ("fixture.disabled", "assistant_model_disabled"),
        ("fixture.absent", "assistant_model_not_found"),
    ] {
        let (status, value) = server
            .json(
                Method::POST,
                SETTINGS,
                Some(
                    json!({"expected_revision":0,"enabled":true,"model":model,"permission":"full"}),
                ),
                server.admin_key(),
            )
            .await;
        assert!(status.is_client_error(), "{value}");
        assert_eq!(value["error"]["code"], code);
    }
    let (status, stale) = server
        .json(
            Method::POST,
            SETTINGS,
            Some(json!({"expected_revision":5,"enabled":true,"model":"fixture.one","permission":"full"})),
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["error"]["code"], "revision_conflict");
    assert_eq!(settings(&server).await, initial);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert!(saved.get("barista").is_none(), "{saved}");

    let enabled = configure(&server, true, "full").await;
    assert_eq!(enabled["enabled"], true);
    assert_eq!(enabled["model"], "fixture.one");
    assert_eq!(enabled["revision"], 1);
    assert_eq!(names(&enabled).len(), 38);
    assert_eq!(enabled["identity"]["sha256"].as_str().unwrap().len(), 64);
    assert_eq!(enabled["prompt"]["version"], 2);
    assert_eq!(enabled["pending"].as_array().unwrap().len(), 5);
    let saved: Value =
        serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    assert_eq!(
        saved["barista"],
        json!({"enabled":true,"model":"fixture.one","permission":"full","revision":1})
    );
    assert_eq!(server.model("fixture.one").await["loaded"], false);
}

#[tokio::test]
async fn dispatch_rechecks_enablement_permission_and_revision_on_every_call() {
    let server = Server::new(true).await;
    let full = configure(&server, true, "full").await;
    let revision = full["revision"].clone();
    let before = server.model("fixture.two").await;
    let (status, changed) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        json!({"model":"fixture.two","policy":"hot","expected_revision":before["revision"]}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["ok"], true);
    assert_eq!(changed["data"]["model"]["policy"], "hot");
    assert_eq!(
        changed["changed"],
        json!([{"target":"fixture.two","field":"policy","before":"on-demand","after":"hot"}])
    );

    let ask = configure(&server, true, "ask").await;
    assert!(
        ask["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["requires_confirmation"]
                == !tool["annotations"]["readOnlyHint"].as_bool().unwrap())
    );
    let (status, stale) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_list_models",
        json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["error"]["code"], "assistant_settings_changed");
    let revision = ask["revision"].clone();
    let current = server.model("fixture.two").await;
    let change =
        json!({"model":"fixture.two","policy":"on-demand","expected_revision":current["revision"]});
    let (status, unconfirmed) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        change.clone(),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        unconfirmed["error"]["code"],
        "assistant_confirmation_required"
    );
    assert_eq!(server.model("fixture.two").await["policy"], "hot");
    let (status, confirmed) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        change,
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{confirmed}");
    assert_eq!(server.model("fixture.two").await["policy"], "on-demand");

    let read_only = configure(&server, true, "read_only").await;
    let listed = names(&read_only);
    assert_eq!(listed.len(), 18);
    assert!(!listed.iter().any(|name| name == "juiced_set_policy"));
    let revision = read_only["revision"].clone();
    let current = server.model("fixture.two").await;
    let (status, refused) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        json!({"model":"fixture.two","policy":"hot","expected_revision":current["revision"]}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(refused["error"]["code"], "assistant_read_only");
    assert_eq!(server.model("fixture.two").await, current);
    let (status, read) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_list_models",
        json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(read["ok"], true);

    let off = configure(&server, false, "read_only").await;
    assert_eq!(off["tools"], json!([]));
    let (status, revoked) = call(
        &server,
        server.admin_key(),
        &off["revision"],
        "juiced_list_models",
        json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(revoked["error"]["code"], "assistant_disabled");
}

#[tokio::test]
async fn tools_report_catalog_and_keep_native_revision_conflicts() {
    let server = Server::new(true).await;
    let revision = configure(&server, true, "full").await["revision"].clone();
    let (_, models) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_list_models",
        json!({}),
        false,
    )
    .await;
    assert_eq!(models["ok"], true, "{models}");
    let one = models["data"]["models"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "fixture.two")
        .unwrap()
        .clone();
    assert_eq!(one["policy"], "on-demand");
    assert_eq!(one["task"], "chat");
    let model_revision = one["revision"].as_u64().unwrap();

    let (status, stale) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        json!({"model":"fixture.two","policy":"disabled","expected_revision":model_revision + 7}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stale["ok"], false, "{stale}");
    assert_eq!(stale["error"]["code"], "revision_conflict");
    assert_eq!(stale["error"]["status"], 409);
    assert_eq!(stale["next"], json!(["juiced_list_models"]));
    assert_eq!(server.model("fixture.two").await["policy"], "on-demand");

    let (_, invalid) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        json!({"model":"fixture.one","policy":"always","expected_revision":0}),
        false,
    )
    .await;
    assert_eq!(invalid["ok"], false);
    assert_eq!(invalid["error"]["code"], "invalid_arguments");
    let (status, unknown) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_shell",
        json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(unknown["error"]["code"], "unknown_tool");

    let (_, status_view) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_status",
        json!({}),
        false,
    )
    .await;
    let summary = status_view["summary"].as_str().unwrap();
    assert!(summary.starts_with("Juiced "), "{summary}");
    let status_view = &status_view["data"];
    assert_eq!(status_view["memory"]["budget_bytes"], 128);
    assert!(status_view["loaded_models"].is_array());
    assert!(status_view["host"]["os"].is_string(), "{status_view}");

    let (_, details) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_model_details",
        json!({"model":"fixture.two"}),
        false,
    )
    .await;
    assert_eq!(details["ok"], true, "{details}");
    assert_eq!(
        details["data"]["settings"]["saved"]["context_tokens"],
        Value::Null
    );
    let (_, missing) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_model_details",
        json!({"model":"fixture/absent model"}),
        false,
    )
    .await;
    assert_eq!(missing["ok"], false, "{missing}");
    assert_eq!(missing["error"]["status"], 404);
}

async fn hub_server(hub: &Hub) -> Server {
    let mut server = Server::prepare(true, |config| {
        config["artifact_source_policy"] = json!("allow-loopback-http");
    });
    server.set_environment("JUICED_HUGGINGFACE_URL", &hub.base);
    server.set_environment("HF_TOKEN", HUB_TOKEN);
    server.start().await;
    server
}

/// One safe argument set per tool: reads as they are, changes as previews.
fn sweep() -> Vec<(&'static str, Value)> {
    vec![
        ("juiced_status", json!({})),
        ("juiced_hardware", json!({})),
        ("juiced_list_models", json!({"task":"chat","limit":2})),
        ("juiced_model_details", json!({"model":"fixture.one"})),
        ("juiced_list_engines", json!({})),
        ("juiced_requests", json!({"limit":5})),
        (
            "juiced_logs",
            json!({"source":"service","level":"info","limit":5}),
        ),
        ("juiced_storage", json!({})),
        ("juiced_cache", json!({"model":"fixture.one"})),
        ("juiced_connections", json!({})),
        ("juiced_search_models", json!({"query":"Process","limit":2})),
        (
            "juiced_inspect_repo",
            json!({"repo":"fixture/Process-GGUF"}),
        ),
        (
            "juiced_fit",
            json!({"repo":"fixture/Process-GGUF","quant":"Q8_0"}),
        ),
        ("juiced_list_local_files", json!({})),
        (
            "juiced_download",
            json!({"repo":"fixture/Process-GGUF","quant":"Q8_0","preview":true}),
        ),
        (
            "juiced_add_model",
            json!({"path":"/nonexistent/model.gguf","preview":true}),
        ),
        (
            "juiced_install_engine",
            json!({"engine":"llama.cpp","backend":"cpu","preview":true}),
        ),
        (
            "juiced_wait_job",
            json!({"id":"pull:0123456789abcdef","max_seconds":1}),
        ),
        (
            "juiced_cancel_job",
            json!({"id":"pull:0123456789abcdef","preview":true}),
        ),
        (
            "juiced_set_policy",
            json!({"model":"fixture.two","policy":"hot","preview":true}),
        ),
        ("juiced_load", json!({"model":"fixture.two","preview":true})),
        (
            "juiced_unload",
            json!({"model":"fixture.two","preview":true}),
        ),
        (
            "juiced_set_model_settings",
            json!({"model":"fixture.two","changes":{"context_tokens":2048},"preview":true}),
        ),
        ("juiced_set_cache", json!({"mode":"memory","preview":true})),
        (
            "juiced_set_protocol",
            json!({"protocol":"ollama","enabled":false,"preview":true}),
        ),
        (
            "juiced_set_alias",
            json!({"alias":"assistant","target":"fixture.two","preview":true}),
        ),
        ("juiced_set_wyoming", json!({"enabled":true,"preview":true})),
        (
            "juiced_create_api_key",
            json!({"name":"SillyTavern","scope":"inference","preview":true}),
        ),
        ("juiced_test_model", json!({"model":"fixture.one"})),
        (
            "juiced_benchmark",
            json!({"model":"fixture.one","preview":true}),
        ),
        (
            "juiced_remove_model",
            json!({"model":"fixture.disabled","preview":true}),
        ),
        (
            "juiced_delete_files",
            json!({"files":["unreferenced.gguf"],"preview":true}),
        ),
        ("juiced_forget_sessions", json!({"all":true,"preview":true})),
        (
            "juiced_uninstall_engine",
            json!({"engine":"llama.cpp","backend":"cpu","preview":true}),
        ),
        (
            "juiced_pause_service",
            json!({"paused":true,"preview":true}),
        ),
        (
            "juiced_restart_model",
            json!({"model":"fixture.two","preview":true}),
        ),
        ("juiced_help", json!({"topic":"increase-context"})),
        ("juiced_docs", json!({"query":"loading policy"})),
    ]
}

async fn observable_state(server: &Server) -> Value {
    let catalog = server.get("juiced/catalog").await;
    let models: Vec<Value> = catalog["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|model| model["id"] != "fixture.one")
        .map(|model| {
            json!([
                model["id"],
                model["policy"],
                model["revision"],
                model["loaded"]
            ])
        })
        .collect();
    json!({
        "models": models,
        "aliases": server.get("juiced/aliases").await["revision"],
        "sessions": server.get("juiced/cache/sessions").await["configuration_generation"],
        "protocols": server.get("juiced/protocols").await,
        "paused": server.get("juiced/admission").await["paused"],
        "keys": server.get("juiced/access").await["keys"].as_array().unwrap().len(),
        "pulls": server.get("juiced/library/pulls").await["data"].as_array().unwrap().len(),
        "wyoming": server.get("juiced/wyoming").await["settings"],
        "engines": server.get("juiced/engines").await["installed"].as_array().unwrap().len(),
    })
}

#[tokio::test]
async fn every_tool_answers_with_a_bounded_redacted_envelope_and_previews_change_nothing() {
    let hub = Hub::start().await;
    let server = hub_server(&hub).await;
    let view = configure(&server, true, "full").await;
    let revision = view["revision"].clone();
    let mut listed = names(&view);
    let mut covered: Vec<String> = sweep().iter().map(|(name, _)| (*name).to_owned()).collect();
    listed.sort();
    covered.sort();
    assert_eq!(listed, covered, "every listed tool needs a sweep entry");
    for tool in view["tools"].as_array().unwrap() {
        let schema = &tool["inputSchema"];
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["additionalProperties"], false);
        let mutating = tool["annotations"]["readOnlyHint"] == false;
        assert_eq!(tool["previewable"], mutating, "{}", tool["name"]);
        assert!(["working", "partial", "pending"].contains(&tool["status"].as_str().unwrap()));
    }

    let before = observable_state(&server).await;
    for (name, arguments) in sweep() {
        let (status, envelope) = call(
            &server,
            server.admin_key(),
            &revision,
            name,
            arguments.clone(),
            false,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{name}: {envelope}");
        assert_eq!(envelope["tool"], name, "{envelope}");
        assert!(envelope["ok"].is_boolean(), "{name}: {envelope}");
        assert!(
            envelope["summary"]
                .as_str()
                .is_some_and(|summary| !summary.is_empty()),
            "{name}: {envelope}"
        );
        assert!(envelope["source"].is_array(), "{name}: {envelope}");
        let text = envelope.to_string();
        assert!(
            text.len() <= RESULT_BYTES,
            "{name} returned {} bytes",
            text.len()
        );
        assert!(!text.contains(HUB_TOKEN), "{name} leaked the hub token");
        assert!(
            !text.contains(support::ADMIN_KEY),
            "{name} leaked the admin key"
        );
        if arguments.get("preview") == Some(&json!(true)) && envelope["ok"] == true {
            assert_eq!(envelope["preview"], true, "{name}: {envelope}");
        }
        if envelope["ok"] == false {
            assert!(envelope["error"]["code"].is_string(), "{name}: {envelope}");
        }
    }
    assert_eq!(
        observable_state(&server).await,
        before,
        "a preview or read changed state"
    );

    for (name, reason) in [
        ("juiced_storage", "helper-storage"),
        ("juiced_delete_files", "helper-storage"),
    ] {
        let (_, pending) = call(
            &server,
            server.admin_key(),
            &revision,
            name,
            json!({}),
            false,
        )
        .await;
        assert_eq!(pending["ok"], false);
        assert_eq!(pending["error"]["code"], "not_supported");
        assert!(
            pending["summary"].as_str().unwrap().contains(reason),
            "{pending}"
        );
    }
    let (_, files) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_remove_model",
        json!({"model":"fixture.disabled","delete_files":true}),
        false,
    )
    .await;
    assert_eq!(files["error"]["code"], "not_supported", "{files}");
    let (_, settings) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_model_settings",
        json!({"model":"fixture.two","changes":{"sampling":{"temperature":0.2}}}),
        false,
    )
    .await;
    assert_eq!(settings["error"]["code"], "not_supported", "{settings}");
    assert!(settings["summary"].as_str().unwrap().contains("helper-b6"));

    let (_, help) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_help",
        json!({}),
        false,
    )
    .await;
    assert_eq!(
        help["data"]["topics"].as_array().unwrap().len(),
        view["prompt"]["skills"].as_array().unwrap().len()
    );
    let (_, unknown) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_help",
        json!({"topic":"shell"}),
        false,
    )
    .await;
    assert_eq!(unknown["error"]["code"], "unknown_topic");
    let (_, page) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_docs",
        json!({"page":"guide/loading-policy"}),
        false,
    )
    .await;
    assert_eq!(page["data"]["console"], "Loading policy", "{page}");
}

#[tokio::test]
async fn permission_levels_gate_every_change_but_never_previews() {
    let server = Server::new(true).await;
    let changes: Vec<(&str, Value)> = sweep()
        .into_iter()
        .filter(|(_, arguments)| arguments.get("preview").is_some())
        .collect();
    assert_eq!(changes.len(), 20);
    for permission in ["read_only", "ask"] {
        let revision = configure(&server, true, permission).await["revision"].clone();
        let before = observable_state(&server).await;
        for (name, arguments) in &changes {
            let mut apply = arguments.clone();
            apply.as_object_mut().unwrap().remove("preview");
            let (status, refused) =
                call(&server, server.admin_key(), &revision, name, apply, false).await;
            assert_eq!(
                status,
                StatusCode::FORBIDDEN,
                "{permission} {name}: {refused}"
            );
            let code = if permission == "ask" {
                "assistant_confirmation_required"
            } else {
                "assistant_read_only"
            };
            assert_eq!(refused["error"]["code"], code, "{name}");
            let (status, preview) = call(
                &server,
                server.admin_key(),
                &revision,
                name,
                arguments.clone(),
                false,
            )
            .await;
            assert_eq!(
                status,
                StatusCode::OK,
                "{permission} {name} preview: {preview}"
            );
        }
        assert_eq!(
            observable_state(&server).await,
            before,
            "{permission} changed state"
        );
    }
    let revision = configure(&server, true, "ask").await["revision"].clone();
    let current = server.model("fixture.two").await;
    let (status, approved) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        json!({"model":"fixture.two","policy":"disabled"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{approved}");
    assert_eq!(approved["changed"][0]["before"], current["policy"]);
    assert_eq!(server.model("fixture.two").await["policy"], "disabled");

    let revision = configure(&server, true, "read_only").await["revision"].clone();
    let (_, test) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_test_model",
        json!({"model":"fixture.one"}),
        false,
    )
    .await;
    assert_eq!(test["error"]["code"], "assistant_read_only", "{test}");
    assert_eq!(server.model("fixture.one").await["loaded"], false);
}

#[tokio::test]
async fn revocation_and_key_classes_stop_dispatch() {
    let server = Server::new(true).await;
    let first = configure(&server, true, "full").await["revision"].clone();
    let (status, _) = call(
        &server,
        server.admin_key(),
        &first,
        "juiced_status",
        json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let second = configure(&server, true, "ask").await["revision"].clone();
    let (status, stale) = call(
        &server,
        server.admin_key(),
        &first,
        "juiced_set_policy",
        json!({"model":"fixture.two","policy":"hot"}),
        true,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(stale["error"]["code"], "assistant_settings_changed");
    assert_eq!(server.model("fixture.two").await["policy"], "on-demand");
    for key in [server.read_key(), None, Some("juiced_not-a-real-key")] {
        let (status, _) = call(&server, key, &second, "juiced_status", json!({}), false).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        let (status, _) = server
            .json(Method::POST, "/v1/juiced/barista/handles/h_0123", None, key)
            .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }
    let off = configure(&server, false, "full").await;
    let (status, disabled) = call(
        &server,
        server.admin_key(),
        &off["revision"],
        "juiced_status",
        json!({}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(disabled["error"]["code"], "assistant_disabled");
}

#[tokio::test]
async fn barista_protects_its_own_model_and_protocol() {
    let server = Server::new(true).await;
    let revision = configure(&server, true, "full").await["revision"].clone();
    for (name, arguments) in [
        (
            "juiced_set_policy",
            json!({"model":"fixture.one","policy":"disabled"}),
        ),
        ("juiced_remove_model", json!({"model":"fixture.one"})),
    ] {
        let (_, refused) = call(
            &server,
            server.admin_key(),
            &revision,
            name,
            arguments,
            false,
        )
        .await;
        assert_eq!(refused["error"]["code"], "assistant_own_model", "{refused}");
    }
    let (_, refused) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_protocol",
        json!({"protocol":"openai","enabled":false}),
        false,
    )
    .await;
    assert_eq!(refused["error"]["code"], "assistant_own_protocol");
    assert_eq!(server.model("fixture.one").await["policy"], "on-demand");
}

#[tokio::test]
async fn created_key_secrets_reach_only_the_operator_through_a_one_time_handle() {
    let server = Server::new(true).await;
    let revision = configure(&server, true, "full").await["revision"].clone();
    let (status, created) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_create_api_key",
        json!({"name":"SillyTavern","scope":"inference"}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["ok"], true, "{created}");
    let handle = created["data"]["secret_handle"]
        .as_str()
        .unwrap()
        .to_owned();
    let text = created.to_string();
    assert!(!text.contains("\"secret\""), "{text}");
    let (status, revealed) = server
        .json(
            Method::POST,
            &format!("/v1/juiced/barista/handles/{handle}"),
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let secret = revealed["secret"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("juiced_") && secret.len() == 50);
    assert!(!text.contains(&secret));
    assert!(text.contains(&secret[..13]), "the key prefix stays visible");
    let (status, _) = server
        .json(Method::GET, "/v1/models", None, Some(secret.as_str()))
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, again) = server
        .json(
            Method::POST,
            &format!("/v1/juiced/barista/handles/{handle}"),
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(again["error"]["code"], "handle_expired");
    let (_, connections) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_connections",
        json!({}),
        false,
    )
    .await;
    assert_eq!(connections["data"]["access"]["keys"], 3, "{connections}");
    assert!(!connections.to_string().contains(&secret));

    let open = Server::new(false).await;
    let revision = configure(&open, true, "full").await["revision"].clone();
    let (_, refused) = call(
        &open,
        None,
        &revision,
        "juiced_create_api_key",
        json!({"name":"first","scope":"operator"}),
        false,
    )
    .await;
    assert_eq!(refused["error"]["code"], "first_key_from_connect");
    assert_eq!(open.get("juiced/access").await["keys"], json!([]));
}

#[tokio::test]
async fn previews_report_the_change_and_applies_report_before_and_after() {
    let server = Server::new(true).await;
    let revision = configure(&server, true, "full").await["revision"].clone();
    let (_, preview) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_alias",
        json!({"alias":"assistant","target":"fixture.two","preview":true}),
        false,
    )
    .await;
    assert_eq!(preview["preview"], true);
    assert_eq!(
        preview["changed"],
        json!([{"target":"assistant","field":"target","before":null,"after":"fixture.two"}])
    );
    assert_eq!(server.get("juiced/aliases").await["aliases"], json!([]));
    let (_, applied) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_alias",
        json!({"alias":"assistant","target":"fixture.two"}),
        false,
    )
    .await;
    assert_eq!(applied["ok"], true, "{applied}");
    assert!(applied.get("preview").is_none());
    assert_eq!(applied["changed"], preview["changed"]);
    let (_, blocked) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_remove_model",
        json!({"model":"fixture.two","preview":true}),
        false,
    )
    .await;
    assert!(
        blocked["data"]["blockers"].as_array().unwrap().len() >= 2,
        "{blocked}"
    );
    let (_, test) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_test_model",
        json!({"model":"fixture.two"}),
        false,
    )
    .await;
    assert_eq!(test["ok"], true, "{test}");
    assert_eq!(test["data"]["kind"], "chat");
    assert_eq!(test["data"]["cold_start"], true);
    assert!(test["data"]["record"]["elapsed_ms"].is_u64(), "{test}");
    let (_, records) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_requests",
        json!({"model":"fixture.two"}),
        false,
    )
    .await;
    assert_eq!(
        records["data"]["records"].as_array().unwrap().len(),
        1,
        "{records}"
    );
    let (_, settings) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_model_settings",
        json!({"model":"fixture.two","changes":{"context_tokens":2048},"expected_revision":99}),
        false,
    )
    .await;
    assert_eq!(settings["ok"], false);
    assert!(
        matches!(
            settings["error"]["code"].as_str(),
            Some("revision_conflict" | "typed_options_unsupported")
        ),
        "{settings}"
    );
}

#[tokio::test]
async fn mcp_gateway_offers_only_inference_tools_and_refuses_browser_origins() {
    let server = Server::new(false).await;
    let rpc = |method: &str, params: Value| json!({"jsonrpc":"2.0","id":"barista","method":method,"params":params});
    let (status, listed) = server
        .json(
            Method::POST,
            "/mcp",
            Some(rpc("tools/list", json!({}))),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let names: Vec<_> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "lemonade_list_models",
            "lemonade_chat",
            "lemonade_complete",
            "lemonade_embed",
            "lemonade_transcribe_audio",
            "lemonade_generate_image",
            "lemonade_docs"
        ]
    );
    let (_, refused) = server
        .json(
            Method::POST,
            "/mcp",
            Some(rpc(
                "tools/call",
                json!({"name":"juiced_list_models","arguments":{}}),
            )),
            None,
        )
        .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(
        refused["result"]["content"][0]["text"],
        "Unknown or unsupported inference tool."
    );
    let response = server
        .client
        .post(format!("{}/mcp", server.base))
        .header("origin", &server.base)
        .json(&rpc("tools/list", json!({})))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}
