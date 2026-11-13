mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::Server;

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
    assert_eq!(names(&enabled).len(), 14);
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
    assert_eq!(changed["result"]["model"]["policy"], "hot");

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
    assert_eq!(listed.len(), 7);
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
    let one = models["result"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "fixture.one")
        .unwrap()
        .clone();
    assert_eq!(one["policy"], "on-demand");
    let model_revision = one["revision"].as_u64().unwrap();

    let (status, stale) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_set_policy",
        json!({"model":"fixture.one","policy":"disabled","expected_revision":model_revision + 7}),
        false,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(stale["ok"], false, "{stale}");
    assert_eq!(stale["result"]["error"]["code"], "revision_conflict");
    assert_eq!(stale["result"]["error"]["status"], 409);
    assert_eq!(server.model("fixture.one").await["policy"], "on-demand");

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
    assert_eq!(invalid["result"]["error"]["code"], "invalid_arguments");
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
    let status_view = &status_view["result"];
    assert_eq!(status_view["memory"]["resident_budget_bytes"], 128);
    assert!(status_view["loaded_models"].is_array());
    assert!(status_view["host"]["os"].is_string(), "{status_view}");

    let (_, settings_view) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_model_settings",
        json!({"model":"fixture.two"}),
        false,
    )
    .await;
    assert_eq!(settings_view["ok"], true, "{settings_view}");
    assert_eq!(settings_view["result"]["settings"]["model"], "fixture.two");
    let (_, missing) = call(
        &server,
        server.admin_key(),
        &revision,
        "juiced_model_settings",
        json!({"model":"fixture/absent model"}),
        false,
    )
    .await;
    assert_eq!(missing["ok"], false, "{missing}");
    assert_eq!(missing["result"]["error"]["status"], 404);
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
