mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server, gguf};

const GIB: u64 = 1024 * 1024 * 1024;
const MIB: u64 = 1024 * 1024;

async fn server(weights: Vec<u8>, args: &[&str], edit: impl FnOnce(&mut Value)) -> Server {
    let mut server = Server::prepare(true, |config| {
        let model = &mut config["models"][0];
        model["provider"] = json!("llama-cpp");
        model["model_file"] = json!("model.gguf");
        model["max_inflight"] = json!(1);
        model["args"]
            .as_array_mut()
            .unwrap()
            .extend(args.iter().map(|argument| json!(argument)));
        edit(config);
    });
    std::fs::write(server.directory.path().join("model.gguf"), weights).unwrap();
    server.start().await;
    server
}

async fn admin(server: &Server, path: &str) -> Value {
    let (status, body) = server
        .json(
            Method::GET,
            &format!("/v1/{path}"),
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{path}: {body}");
    body
}

async fn suitability(server: &Server) -> Value {
    admin(server, "juiced/suitability?model=fixture.one").await["resources"].clone()
}

fn sum(breakdown: &Value) -> u64 {
    [
        "weights_bytes",
        "context_bytes",
        "compute_bytes",
        "projector_bytes",
        "draft_bytes",
        "other_bytes",
    ]
    .iter()
    .map(|field| breakdown[field].as_u64().unwrap())
    .sum()
}

fn legacy(file: u64, blocks: u64, kv_heads: u64, width: u64, context: u64) -> u64 {
    file + blocks * context * kv_heads * width * 2 + 256 * MIB
}

#[tokio::test]
async fn fit_preview_reports_the_layout_budget_and_unsaved_drafts_under_all_prefixes() {
    let weights = gguf::dense();
    let file = weights.len() as u64;
    let server = server(
        weights,
        &[
            "--ctx-size",
            "32768",
            "--gpu-layers",
            "0",
            "--parallel",
            "1",
        ],
        |config| {
            config["resident_budget_bytes"] = json!(64 * GIB);
            config["models"][0]["resident_bytes"] = json!(GIB);
        },
    )
    .await;
    for prefix in PREFIXES {
        let (status, fit) = server
            .json(
                Method::GET,
                &format!("{prefix}/models/fixture.one/fit"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{fit}");
        assert_eq!(fit["source"], "estimate");
        let breakdown = &fit["breakdown"];
        assert_eq!(breakdown["weights_bytes"], file);
        assert_eq!(breakdown["context_bytes"], 54u64 * 32768 * 8 * 256 * 2);
        assert_eq!(breakdown["projector_bytes"], 0);
        assert_eq!(breakdown["draft_bytes"], 0);
        assert_eq!(breakdown["other_bytes"], 1024 * MIB);
        assert_eq!(fit["total_bytes"], sum(breakdown));
        assert_eq!(fit["reservation_bytes"], GIB);
        assert_eq!(fit["budget_bytes"], 64 * GIB);
        assert_eq!(fit["available_bytes"], 64 * GIB);
        assert_eq!(fit["loaded"], false);
        assert_eq!(fit["fits"], true);
        assert_eq!(fit["trained_context"], 131_072);
        assert_eq!(fit["largest_context_that_fits"], 131_072);
        assert_eq!(fit["plan"]["gpu"], false);
        assert_eq!(fit["plan"]["cache_type_k"], "f16");
        assert_eq!(fit["draft"], false);
        assert!(
            fit["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"] == "reservation_differs"),
            "{fit}"
        );
    }
    let draft = admin(
        &server,
        "models/fixture.one/fit?context_tokens=65536&kv_cache_type=q8_0&parallel_slots=1",
    )
    .await;
    assert_eq!(draft["draft"], true);
    assert_eq!(draft["plan"]["context_tokens"], 65536);
    assert_eq!(draft["plan"]["cache_type_v"], "q8_0");
    assert_eq!(
        draft["breakdown"]["context_bytes"],
        54u64 * 65536 * 2 * (8 * 128 * 34 / 32)
    );
    assert_eq!(
        server.get("models/fixture.one/options").await["saved"]["context_tokens"],
        Value::Null
    );
    for query in ["kv_cache_type=q2_k", "context_tokens=0", "unknown=1"] {
        let (status, error) = server
            .json(
                Method::GET,
                &format!("/v1/models/fixture.one/fit?{query}"),
                None,
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query}: {error}");
    }
    let (status, _) = server
        .json(
            Method::GET,
            "/v1/models/fixture.one/fit",
            None,
            server.read_key(),
        )
        .await;
    assert!(status.is_client_error());
    let (status, missing) = server
        .json(
            Method::GET,
            "/v1/models/missing/fit",
            None,
            server.admin_key(),
        )
        .await;
    assert!(status.is_client_error(), "{missing}");
}

#[tokio::test]
async fn an_over_budget_context_reports_the_largest_context_that_fits() {
    let server = server(
        gguf::dense(),
        &[
            "--ctx-size",
            "32768",
            "--gpu-layers",
            "0",
            "--parallel",
            "1",
        ],
        |config| {
            config["resident_budget_bytes"] = json!(8 * GIB);
            config["models"][0]["resident_bytes"] = json!(GIB);
        },
    )
    .await;
    let fit = admin(&server, "models/fixture.one/fit").await;
    assert_eq!(fit["fits"], false, "{fit}");
    let largest = fit["largest_context_that_fits"].as_u64().unwrap();
    assert!(
        largest % 1024 == 0 && (4096..32768).contains(&largest),
        "{fit}"
    );
    let at = admin(
        &server,
        &format!("models/fixture.one/fit?context_tokens={largest}"),
    )
    .await;
    assert_eq!(at["fits"], true, "{at}");
    let next = admin(
        &server,
        &format!("models/fixture.one/fit?context_tokens={}", largest + 1024),
    )
    .await;
    assert_eq!(next["fits"], false, "{next}");
}

#[tokio::test]
async fn a_loaded_model_counts_its_own_reservation_as_available_and_reports_provenance() {
    let server = server(
        gguf::qwen35moe(),
        &[
            "--ctx-size",
            "8192",
            "--gpu-layers",
            "0",
            "--parallel",
            "1",
            "--spec-type",
            "draft-mtp",
            "--metrics",
        ],
        |config| {
            config["resident_budget_bytes"] = json!(3 * GIB);
            config["models"][0]["resident_bytes"] = json!(2 * GIB);
        },
    )
    .await;
    let before = suitability(&server).await;
    assert_eq!(
        before["declared_budget"]["fits_additional_reservation"],
        true
    );
    assert_eq!(before["assessment_scope"], "additional-worker");
    let options = server.get("models/fixture.one/options").await;
    assert_eq!(options["fields"]["context"]["saved"], 8192);
    assert_eq!(options["fields"]["context"]["launched"], Value::Null);
    assert_eq!(options["fields"]["mtp"]["source"], "model");
    assert_eq!(options["fields"]["kv_cache_type"]["source"], "engine");
    assert_eq!(options["fields"]["kv_cache_type"]["effective"], "f16");
    assert_eq!(options["fields"]["flash_attention"]["effective"], "auto");
    assert_eq!(options["fields"]["gpu_layers"]["source"], "saved");
    assert_eq!(options["fields"]["slots"]["source"], "auto");
    assert_eq!(options["mtp_metrics"], Value::Null);

    let (status, loaded) = server.post("load", json!({"model":"fixture.one"})).await;
    assert_eq!(status, StatusCode::OK, "{loaded}");
    #[cfg(target_os = "linux")]
    {
        let pid = server.model("fixture.one").await["pid"].as_u64().unwrap();
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap();
        let arguments: Vec<&[u8]> = bytes.split(|byte| *byte == 0).collect();
        let pairs: Vec<_> = arguments
            .windows(2)
            .filter(|pair| pair[0] == b"--cache-ram")
            .collect();
        assert_eq!(pairs, [[b"--cache-ram".as_slice(), b"1024".as_slice()]]);
    }
    let after = suitability(&server).await;
    assert_eq!(after["declared_budget"]["reserved_bytes"], 2 * GIB);
    assert_eq!(after["declared_budget"]["available_bytes"], 3 * GIB);
    assert_eq!(
        after["declared_budget"]["fits_additional_reservation"],
        true
    );
    assert_eq!(after["assessment_scope"], "replace-running-worker");
    assert!(
        !after["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|blocker| blocker == "resident_budget")
    );
    let fit = admin(&server, "models/fixture.one/fit").await;
    assert_eq!(fit["loaded"], true);
    assert_eq!(fit["available_bytes"], 3 * GIB);

    let options = server.get("models/fixture.one/options").await;
    let context = &options["fields"]["context"];
    assert_eq!(context["launched"], 8192, "{options}");
    assert_eq!(context["effective"], 8192);
    assert_eq!(context["source"], "reported");
    assert_eq!(context["restart_needed"], false);
    assert_eq!(options["fields"]["slots"]["effective"], 1);
    assert_eq!(options["fields"]["slots"]["source"], "reported");
    assert_eq!(options["fields"]["mtp"]["launched"], true);
    let metrics = &options["mtp_metrics"];
    assert_eq!(metrics["draft_tokens"], 105, "{options}");
    assert_eq!(metrics["accepted_tokens"], 48);
    assert_eq!(metrics["drafts"], 35);
    assert!((metrics["acceptance_rate"].as_f64().unwrap() - 48.0 / 105.0).abs() < 1e-9);
}

#[tokio::test]
async fn generated_reservations_are_corrected_and_saved_options_recompute_them() {
    let weights = gguf::qwen35moe();
    let file = weights.len() as u64;
    let generated = legacy(file, 41, 2, 512, 8192);
    let mut server = server(
        weights,
        &["--ctx-size", "8192", "--gpu-layers", "0", "--parallel", "1"],
        |config| {
            config["resident_budget_bytes"] = json!(8 * GIB);
            config["models"][0]["resident_bytes"] = json!(generated);
        },
    )
    .await;
    let corrected = server.model("fixture.one").await["resident_bytes"]
        .as_u64()
        .unwrap();
    assert_ne!(corrected, generated);
    let fit = admin(&server, "models/fixture.one/fit").await;
    assert_eq!(fit["total_bytes"], corrected, "{fit}");
    assert_eq!(fit["reservation_bytes"], corrected);
    assert!(
        !fit["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"] == "reservation_differs")
    );
    // The 10 attention layers and 30 recurrent layers, not 41 full-KV blocks.
    assert_eq!(
        fit["breakdown"]["context_bytes"],
        10u64 * 8192 * 2048 + 30 * 2_195_456
    );

    let current = server.get("models/fixture.one/options").await;
    let (status, saved) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":current["revision"],"options":{"context_tokens":16384,"gpu_layers":0,"parallel_slots":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    let reserved = saved["reservation_bytes"].as_u64().unwrap();
    assert!(reserved > corrected);
    assert_eq!(
        server.model("fixture.one").await["resident_bytes"],
        reserved
    );
    assert_eq!(
        admin(&server, "models/fixture.one/fit").await["total_bytes"],
        reserved
    );

    let (status, refused) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":saved["revision"],"options":{"context_tokens":262144,"gpu_layers":0,"parallel_slots":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"]["code"], "reservation_exceeds_budget");
    assert_eq!(
        server.model("fixture.one").await["resident_bytes"],
        reserved
    );

    server.restart().await;
    assert_eq!(
        server.model("fixture.one").await["resident_bytes"],
        reserved
    );
}

#[tokio::test]
async fn explicit_reservations_and_non_gguf_models_are_left_unchanged() {
    let server = server(
        gguf::qwen35moe(),
        &["--ctx-size", "8192", "--gpu-layers", "0", "--parallel", "1"],
        |config| {
            config["resident_budget_bytes"] = json!(8 * GIB);
            config["models"][0]["resident_bytes"] = json!(3 * GIB);
            config["models"][1]["provider"] = json!("llama-cpp");
        },
    )
    .await;
    assert_eq!(server.model("fixture.one").await["resident_bytes"], 3 * GIB);
    assert_eq!(server.model("fixture.two").await["resident_bytes"], 64);
    let (status, error) = server
        .json(
            Method::GET,
            "/v1/models/fixture.two/fit",
            None,
            server.admin_key(),
        )
        .await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["error"]["code"], "model_metadata_unavailable");
}
