mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::Server;

async fn rpc(server: &Server, key: Option<&str>, method: &str, params: Value) -> Value {
    let (status, value) = server
        .json(
            Method::POST,
            "/mcp",
            Some(json!({"jsonrpc":"2.0","id":"management","method":method,"params":params})),
            key,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["id"], "management");
    value
}

async fn call(server: &Server, key: Option<&str>, name: &str, arguments: Value) -> Value {
    rpc(
        server,
        key,
        "tools/call",
        json!({"name":name,"arguments":arguments}),
    )
    .await
}

fn names(listed: &Value) -> Vec<String> {
    listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn management_tools_are_listed_and_callable_only_with_operator_authority() {
    let server = Server::new(true).await;
    let inference = names(&rpc(&server, server.read_key(), "tools/list", json!({})).await);
    assert_eq!(
        inference,
        [
            "lemonade_list_models",
            "lemonade_chat",
            "lemonade_complete",
            "lemonade_embed"
        ]
    );
    let listed = rpc(&server, server.admin_key(), "tools/list", json!({})).await;
    let operator = names(&listed);
    assert_eq!(&operator[..4], &inference[..]);
    for name in [
        "juiced_status",
        "juiced_list_models",
        "juiced_model_settings",
        "juiced_list_engines",
        "juiced_search_models",
        "juiced_list_local_files",
        "juiced_list_downloads",
        "juiced_set_policy",
        "juiced_load_model",
        "juiced_unload_model",
        "juiced_set_model_settings",
        "juiced_download_model",
        "juiced_add_model",
        "juiced_install_engine",
    ] {
        assert!(operator.iter().any(|listed| listed == name), "{name}");
    }
    let tool = |name: &str| {
        listed["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(
        tool("juiced_list_models")["annotations"]["readOnlyHint"],
        true
    );
    assert_eq!(
        tool("juiced_set_policy")["annotations"]["readOnlyHint"],
        false
    );
    assert_eq!(
        tool("juiced_unload_model")["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        tool("juiced_set_policy")["inputSchema"]["required"],
        json!(["model", "policy", "expected_revision"])
    );

    let before = server.model("fixture.one").await;
    let refused = call(
        &server,
        server.read_key(),
        "juiced_set_policy",
        json!({"model":"fixture.one","policy":"hot","expected_revision":before["revision"]}),
    )
    .await;
    assert_eq!(refused["error"]["code"], -32001, "{refused}");
    assert_eq!(refused["error"]["data"]["code"], "forbidden");
    assert!(refused.get("result").is_none());
    let refused_read = call(&server, server.read_key(), "juiced_list_models", json!({})).await;
    assert_eq!(refused_read["error"]["code"], -32001);
    assert_eq!(server.model("fixture.one").await, before);

    let unknown = call(&server, server.admin_key(), "juiced_shell", json!({})).await;
    assert_eq!(unknown["error"]["code"], -32602);
    let invalid = call(
        &server,
        server.admin_key(),
        "juiced_set_policy",
        json!({"model":"fixture.one","policy":"always","expected_revision":0}),
    )
    .await;
    assert_eq!(invalid["error"]["code"], -32602);
}

#[tokio::test]
async fn read_tool_reports_catalog_and_policy_tool_keeps_revision_conflicts() {
    let server = Server::new(true).await;
    let models = call(&server, server.admin_key(), "juiced_list_models", json!({})).await;
    assert_eq!(models["result"]["isError"], false, "{models}");
    let listed = models["result"]["structuredContent"]["models"]
        .as_array()
        .unwrap();
    let one = listed
        .iter()
        .find(|model| model["id"] == "fixture.one")
        .unwrap();
    assert_eq!(one["policy"], "on-demand");
    assert_eq!(one["loaded"], false);
    let revision = one["revision"].as_u64().unwrap();
    let text: Value =
        serde_json::from_str(models["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(text, models["result"]["structuredContent"]);

    let stale = call(
        &server,
        server.admin_key(),
        "juiced_set_policy",
        json!({"model":"fixture.one","policy":"disabled","expected_revision":revision + 7}),
    )
    .await;
    assert_eq!(stale["result"]["isError"], true, "{stale}");
    assert_eq!(
        stale["result"]["structuredContent"]["error"]["code"],
        "revision_conflict"
    );
    assert_eq!(stale["result"]["structuredContent"]["error"]["status"], 409);
    assert_eq!(server.model("fixture.one").await["policy"], "on-demand");

    let changed = call(
        &server,
        server.admin_key(),
        "juiced_set_policy",
        json!({"model":"fixture.one","policy":"disabled","expected_revision":revision}),
    )
    .await;
    assert_eq!(changed["result"]["isError"], false, "{changed}");
    let model = &changed["result"]["structuredContent"]["model"];
    assert_eq!(model["policy"], "disabled");
    assert!(model["revision"].as_u64().unwrap() > revision);
    assert_eq!(server.model("fixture.one").await["policy"], "disabled");

    let status = call(&server, server.admin_key(), "juiced_status", json!({})).await;
    let status = &status["result"]["structuredContent"];
    assert_eq!(status["memory"]["resident_budget_bytes"], 128);
    assert!(status["loaded_models"].is_array());
    assert!(status["host"]["os"].is_string(), "{status}");

    let settings = call(
        &server,
        server.admin_key(),
        "juiced_model_settings",
        json!({"model":"fixture.two"}),
    )
    .await;
    assert_eq!(settings["result"]["isError"], false, "{settings}");
    assert_eq!(
        settings["result"]["structuredContent"]["settings"]["model"],
        "fixture.two"
    );
    let missing = call(
        &server,
        server.admin_key(),
        "juiced_model_settings",
        json!({"model":"fixture/absent model"}),
    )
    .await;
    assert_eq!(missing["result"]["isError"], true, "{missing}");
    assert_eq!(
        missing["result"]["structuredContent"]["error"]["status"],
        404
    );
}

#[tokio::test]
async fn trusted_lan_lists_management_tools_and_browser_calls_must_be_same_origin() {
    let server = Server::new(false).await;
    let listed = names(&rpc(&server, None, "tools/list", json!({})).await);
    assert_eq!(listed.len(), 18);
    let host = server.base.trim_start_matches("http://").to_owned();
    for (origin, status) in [
        (server.base.clone(), StatusCode::OK),
        ("http://evil.example".to_owned(), StatusCode::FORBIDDEN),
        (format!("http://{host}.evil.example"), StatusCode::FORBIDDEN),
    ] {
        let response = server
            .client
            .post(format!("{}/mcp", server.base))
            .header("origin", &origin)
            .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"juiced_list_models","arguments":{}}}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status, "{origin}");
    }
}
