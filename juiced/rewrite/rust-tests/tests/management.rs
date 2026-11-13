mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{PREFIXES, Server, rejected_manager as rejected_process};

fn registration(server: &Server, id: &str) -> Value {
    let mut spec = server.config["models"][0].clone();
    spec["id"] = json!(id);
    spec["policy"] = json!("disabled");
    spec["revision"] = json!(0);
    spec
}

#[tokio::test]
async fn registration_detail_and_removal_work_under_all_native_prefixes() {
    let server = Server::new(true).await;
    for (index, prefix) in PREFIXES.iter().enumerate() {
        let id = format!("registered.prefix-{index}");
        let spec = registration(&server, &id);
        let (status, created) = server
            .json(
                Method::POST,
                &format!("{prefix}/models/register"),
                Some(spec.clone()),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(created["policy"], "disabled");
        assert_eq!(created["loaded"], false);
        let revision = created["revision"].as_u64().unwrap();
        let (status, detail) = server
            .json(
                Method::GET,
                &format!("{prefix}/models/{id}"),
                None,
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["object"], "model");
        assert_eq!(detail["id"], id);
        assert_eq!(detail["juiced"]["revision"], revision);
        assert_eq!(
            server
                .json(
                    Method::POST,
                    &format!("{prefix}/models/register"),
                    Some(spec),
                    server.admin_key()
                )
                .await
                .0,
            StatusCode::CONFLICT
        );
        let (status, removed) = server
            .json(
                Method::POST,
                &format!("{prefix}/delete"),
                Some(json!({"model":id,"expected_revision":revision})),
                server.admin_key(),
            )
            .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(removed["registration_removed"], true);
        assert_eq!(removed["weights_retained"], true);
        assert_eq!(
            server
                .json(
                    Method::GET,
                    &format!("{prefix}/models/{id}"),
                    None,
                    server.read_key()
                )
                .await
                .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            server
                .json(
                    Method::POST,
                    &format!("{prefix}/juiced/admission"),
                    Some(json!({"paused":true})),
                    server.admin_key()
                )
                .await
                .1["paused"],
            true
        );
        assert_eq!(
            server
                .json(
                    Method::GET,
                    &format!("{prefix}/juiced/admission"),
                    None,
                    server.admin_key()
                )
                .await
                .1["paused"],
            true
        );
        assert_eq!(
            server
                .json(
                    Method::POST,
                    &format!("{prefix}/juiced/admission"),
                    Some(json!({"paused":false})),
                    server.admin_key()
                )
                .await
                .1["paused"],
            false
        );
    }
}

#[tokio::test]
async fn new_models_reject_invalid_or_enabled_registration_without_persisting() {
    let server = Server::new(true).await;
    let before_file = std::fs::read(&server.config_path).unwrap();
    let before = server.get("juiced/catalog").await;
    for change in [
        "hot",
        "on-demand",
        "revision",
        "slots",
        "budget",
        "identity",
        "arguments",
    ] {
        let mut spec = registration(&server, "invalid.registration");
        match change {
            "hot" | "on-demand" => spec["policy"] = json!(change),
            "revision" => spec["revision"] = json!(17),
            "slots" => spec["max_inflight"] = json!(0),
            "budget" => spec["resident_bytes"] = json!(129),
            "identity" => spec["id"] = json!(""),
            _ => spec["args"] = json!([]),
        }
        assert_eq!(
            server.post("models/register", spec).await.0,
            StatusCode::BAD_REQUEST,
            "invalid {change} registration accepted"
        );
    }
    assert!(
        server
            .post("models/register", json!({"id":"incomplete"}))
            .await
            .0
            .is_client_error()
    );
    assert_eq!(server.get("juiced/catalog").await, before);
    assert_eq!(std::fs::read(&server.config_path).unwrap(), before_file);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[tokio::test]
async fn registrations_survive_restart_and_id_reuse_cannot_accept_an_old_revision() {
    let mut server = Server::new(true).await;
    let spec = registration(&server, "dynamic.model");
    let file = spec["model_file"].as_str().unwrap().to_owned();
    let original_weights = std::fs::read(&file).unwrap();
    let (status, first) = server.post("models/register", spec.clone()).await;
    assert_eq!(status, StatusCode::OK);
    let first_revision = first["revision"].as_u64().unwrap();
    server.restart().await;
    let first_restored = server.model("dynamic.model").await;
    assert_eq!(first_restored["revision"], first_revision);
    assert_eq!(first_restored["policy"], "disabled");
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":"dynamic.model","expected_revision":first_revision+1})
            )
            .await
            .1["error"]["code"],
        "revision_conflict"
    );
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":"dynamic.model","expected_revision":first_revision})
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(std::fs::read(&file).unwrap(), original_weights);
    server.restart().await;
    assert_eq!(
        server
            .json(
                Method::GET,
                "/v1/models/dynamic.model",
                None,
                server.read_key()
            )
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    let (status, replacement) = server.post("models/register", spec).await;
    assert_eq!(status, StatusCode::OK);
    let replacement_revision = replacement["revision"].as_u64().unwrap();
    assert!(replacement_revision > first_revision);
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":"dynamic.model","expected_revision":first_revision})
            )
            .await
            .1["error"]["code"],
        "revision_conflict"
    );
    assert_eq!(server.post("juiced/policy", json!({"model":"dynamic.model","policy":"on-demand","expected_revision":first_revision})).await.1["error"]["code"], "revision_conflict");
    server.restart().await;
    assert_eq!(
        server.model("dynamic.model").await["revision"],
        replacement_revision
    );
    assert_eq!(std::fs::read(&file).unwrap(), original_weights);
}

#[tokio::test]
async fn removing_a_registration_requires_disabled_unloaded_state_and_retains_weights() {
    let server = Server::new(true).await;
    let spec = registration(&server, "guarded.model");
    let file = spec["model_file"].as_str().unwrap().to_owned();
    let original = std::fs::read(&file).unwrap();
    let (_, created) = server.post("models/register", spec).await;
    let revision = created["revision"].as_u64().unwrap();
    let (status, enabled) = server
        .post(
            "juiced/policy",
            json!({"model":"guarded.model","policy":"on-demand","expected_revision":revision}),
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    let enabled_revision = enabled["revision"].as_u64().unwrap();
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":"guarded.model","expected_revision":enabled_revision})
            )
            .await
            .1["error"]["code"],
        "model_in_use"
    );
    assert_eq!(
        server
            .post("load", json!({"model":"guarded.model"}))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":"guarded.model","expected_revision":enabled_revision})
            )
            .await
            .1["error"]["code"],
        "model_in_use"
    );
    let (status, disabled) = server.post("juiced/policy", json!({"model":"guarded.model","policy":"disabled","expected_revision":enabled_revision})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(disabled["loaded"], false);
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":"guarded.model","expected_revision":disabled["revision"]})
            )
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(std::fs::read(&file).unwrap(), original);
    assert!(
        server.model("fixture.one").await["downloaded"]
            .as_bool()
            .unwrap()
    );
}

#[tokio::test]
async fn maintenance_rejects_new_work_drains_existing_work_and_preserves_intent() {
    let server = Server::new(true).await;
    assert_eq!(
        server.post("load", json!({"model":"fixture.one"})).await.0,
        StatusCode::OK
    );
    assert_eq!(
        server.post("load", json!({"model":"fixture.two"})).await.0,
        StatusCode::OK
    );
    let original = server.model("fixture.one").await;
    let config = std::fs::read(&server.config_path).unwrap();
    let request = server
        .client
        .post(format!("{}/v1/chat/completions", server.base))
        .bearer_auth(support::CLIENT_KEY)
        .json(&json!({"model":"fixture.one","test_id":"maintenance-active","fixture_delay_ms":250}))
        .send();
    let request = tokio::spawn(request);
    server
        .wait_for(async || server.event("fixture.one", "maintenance-active", "started"))
        .await;
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":true}))
            .await
            .1["paused"],
        true
    );
    assert_eq!(
        server.get("juiced/admission").await["resources"]["active_requests"],
        1
    );
    for (path, body) in [
        ("load", json!({"model":"fixture.two"})),
        ("chat/completions", json!({"model":"fixture.one"})),
    ] {
        let (status, error) = server.post(path, body).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error["error"]["code"], "admission_paused");
    }
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.one"}))
            .await
            .1["error"]["code"],
        "worker_busy"
    );
    assert_eq!(
        server
            .post("unload", json!({"model":"fixture.two"}))
            .await
            .0,
        StatusCode::OK
    );
    let response = request.await.unwrap().unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let _: Value = response.json().await.unwrap();
    server
        .wait_for(async || {
            server.get("juiced/admission").await["resources"]["active_requests"] == 0
        })
        .await;
    assert_eq!(
        server
            .post("juiced/admission", json!({"paused":false}))
            .await
            .1["paused"],
        false
    );
    let resumed = server.model("fixture.one").await;
    assert_eq!(resumed["pid"], original["pid"]);
    assert_eq!(resumed["policy"], original["policy"]);
    assert_eq!(resumed["revision"], original["revision"]);
    assert_eq!(std::fs::read(&server.config_path).unwrap(), config);
    assert_eq!(
        server
            .post("chat/completions", json!({"model":"fixture.one"}))
            .await
            .0,
        StatusCode::OK
    );
    assert!(
        server
            .post("juiced/admission", json!({"paused":true,"unexpected":true}))
            .await
            .0
            .is_client_error()
    );
}

#[tokio::test]
async fn live_root_cannot_be_reopened_through_alias_or_after_marker_unlink() {
    let mut server = Server::new(false).await;
    rejected_process(&server.directory.path().join(".").join("config.json")).await;
    #[cfg(unix)]
    {
        let aliases = tempfile::tempdir().unwrap();
        let alias = aliases.path().join("alias");
        std::os::unix::fs::symlink(server.directory.path(), &alias).unwrap();
        rejected_process(&alias.join("config.json")).await;
        std::fs::remove_file(server.directory.path().join(".juiced-store.lock")).unwrap();
        rejected_process(&server.config_path).await;
        assert_eq!(server.get("health").await["status"], "ok");
    }
    server.restart().await;
    assert_eq!(server.get("health").await["status"], "ok");
}

#[cfg(unix)]
#[tokio::test]
async fn parent_and_child_claims_exclude_each_other_before_startup() {
    for child_first in [false, true] {
        let mut server = Server::prepare(false, |_| {});
        let parent_config = server.config_path.clone();
        let child_root = server.directory.path().join("nested");
        std::fs::create_dir(&child_root).unwrap();
        let child_config = child_root.join("config.json");
        std::fs::copy(&parent_config, &child_config).unwrap();
        if child_first {
            server.config_path = child_config.clone();
        }
        server.start().await;
        rejected_process(if child_first {
            &parent_config
        } else {
            &child_config
        })
        .await;
        let rejected_root = if child_first {
            server.directory.path()
        } else {
            child_root.as_path()
        };
        assert!(
            !rejected_root.join(".juiced-store.lock").exists(),
            "rejected ownership published a control marker"
        );
        assert_eq!(server.get("health").await["status"], "ok");
        server.stop();
        server.config_path = if child_first {
            parent_config
        } else {
            child_config
        };
        server.start().await;
        assert_eq!(server.get("health").await["status"], "ok");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn a_foreign_child_claim_does_not_create_missing_descendants_of_a_live_root() {
    let server = Server::new(false).await;
    let missing = server.directory.path().join("must-not-create");
    rejected_process(&missing.join("deeper/config.json")).await;
    assert!(!missing.exists());
    assert_eq!(server.get("health").await["status"], "ok");
}

#[tokio::test]
async fn encoded_public_model_names_keep_their_exact_registration_identity() {
    let server = Server::new(true).await;
    let id = "Publisher/Small model · Q4";
    let (status, registered) = server
        .post("models/register", registration(&server, id))
        .await;
    assert_eq!(status, StatusCode::OK);
    let mut url = reqwest::Url::parse(&format!("{}/v1/models/", server.base)).unwrap();
    url.path_segments_mut().unwrap().pop_if_empty().push(id);
    let response = server
        .client
        .get(url)
        .bearer_auth(support::CLIENT_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let detail: Value = response.json().await.unwrap();
    assert_eq!(detail["id"], id);
    assert_eq!(detail["juiced"]["revision"], registered["revision"]);
    assert_eq!(
        server
            .post(
                "delete",
                json!({"model":id,"expected_revision":registered["revision"]})
            )
            .await
            .0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn independent_sibling_roots_keep_running_across_other_manager_release() {
    let mut first = Server::new(false).await;
    let second = Server::new(false).await;
    assert_eq!(
        first.directory.path().parent(),
        second.directory.path().parent()
    );
    assert_eq!(first.get("health").await["status"], "ok");
    assert_eq!(second.get("health").await["status"], "ok");
    first.stop();
    assert_eq!(second.get("health").await["status"], "ok");
    first.start().await;
    assert_eq!(first.get("health").await["status"], "ok");
    assert_eq!(second.get("health").await["status"], "ok");
}

#[tokio::test]
async fn competing_registration_and_removal_writes_have_coherent_single_winners() {
    let server = Server::new(true).await;
    let spec = registration(&server, "raced.registration");
    let (left, right) = tokio::join!(
        server.post("models/register", spec.clone()),
        server.post("models/register", spec)
    );
    assert_ne!(left.0.is_success(), right.0.is_success());
    assert!([left.0, right.0].contains(&StatusCode::CONFLICT));
    let revision = server.model("raced.registration").await["revision"]
        .as_u64()
        .unwrap();
    let (removed, enabled) = tokio::join!(
        server.post(
            "delete",
            json!({"model":"raced.registration","expected_revision":revision})
        ),
        server.post(
            "juiced/policy",
            json!({"model":"raced.registration","policy":"on-demand","expected_revision":revision})
        ),
    );
    assert_ne!(removed.0.is_success(), enabled.0.is_success());
    let disk: Value = serde_json::from_slice(&std::fs::read(&server.config_path).unwrap()).unwrap();
    let persisted = disk["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|model| model["id"] == "raced.registration");
    let (status, detail) = server
        .json(
            Method::GET,
            "/v1/models/raced.registration",
            None,
            server.read_key(),
        )
        .await;
    if removed.0.is_success() {
        assert!(enabled.0.is_client_error());
        assert!(persisted.is_none());
        assert_eq!(status, StatusCode::NOT_FOUND);
    } else {
        assert_eq!(removed.0, StatusCode::CONFLICT);
        assert_eq!(persisted.unwrap()["policy"], "on-demand");
        assert_eq!(status, StatusCode::OK);
        assert_eq!(detail["juiced"]["policy"], "on-demand");
    }
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
}

#[tokio::test]
async fn hot_saved_intent_preloads_after_restart_without_changing_other_models() {
    let mut server = Server::new(true).await;
    let revision = server.model("fixture.one").await["revision"]
        .as_u64()
        .unwrap();
    assert_eq!(
        server
            .post(
                "juiced/policy",
                json!({"model":"fixture.one","policy":"hot","expected_revision":revision})
            )
            .await
            .0,
        StatusCode::OK
    );
    let before = server.model("fixture.one").await;
    assert_eq!(before["loaded"], false);
    server.restart().await;
    server
        .wait_for(async || server.model("fixture.one").await["loaded"] == true)
        .await;
    let after = server.model("fixture.one").await;
    assert_eq!(after["policy"], "hot");
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(server.model("fixture.two").await["loaded"], false);
    assert_eq!(server.model("fixture.disabled").await["loaded"], false);
}

#[tokio::test]
async fn failed_configuration_publication_retains_policy_and_revision_in_the_live_service() {
    let mut server = Server::new(true).await;
    let before = server.model("fixture.one").await;
    let original = std::fs::read(&server.config_path).unwrap();
    std::fs::remove_file(&server.config_path).unwrap();
    std::fs::create_dir(&server.config_path).unwrap();
    let (status, error) = server
        .post(
            "juiced/policy",
            json!({"model":"fixture.one","policy":"hot","expected_revision":before["revision"]}),
        )
        .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(error["error"]["code"], "policy_save");
    assert_eq!(server.model("fixture.one").await, before);
    std::fs::remove_dir(&server.config_path).unwrap();
    std::fs::write(&server.config_path, original).unwrap();
    server.restart().await;
    assert_eq!(server.model("fixture.one").await, before);
}
