mod support;

use juiced_service::{
    artifacts::SourcePolicy,
    config::{Config, ModelSpec, Policy, Provider},
    profiles::LaunchOptions,
};
use reqwest::StatusCode;
use serde_json::{Value, json};
use std::path::PathBuf;
use support::{Server, artifacts::Source, filesystem::tree};

const CONFIG_BYTES: usize = 16 * 1024 * 1024;

fn padding(template: &ModelSpec, id: &str) -> ModelSpec {
    let mut model = template.clone();
    model.id = id.to_owned();
    model.policy = Policy::Disabled;
    model.revision = 0;
    model.artifact = None;
    model.execution_profile.command = "unused-opaque-http-worker".to_owned();
    model.execution_profile.args = vec!["{model}".to_owned(), "{port}".to_owned()];
    model
        .execution_profile
        .args
        .extend((0..16).map(|_| "x".repeat(65528)));
    let bytes: usize = model.execution_profile.args.iter().map(String::len).sum();
    assert!(bytes < 1024 * 1024);
    assert_eq!(model.execution_profile.adapter, Provider::ExternalHttp);
    model
}

fn prepare(count: usize, exact_boundary: bool) -> (Server, Config) {
    let server = Server::prepare(true, |_| {});
    let mut config: Config = serde_json::from_value(server.config.clone()).unwrap();
    config.artifact_source_policy = SourcePolicy::AllowLoopbackHttp;
    let template = config.models.remove(0);
    config.models = (0..count)
        .map(|index| padding(&template, &format!("padding.{index:02}")))
        .collect();
    if exact_boundary {
        let mut options = template;
        options.id = "options.boundary".to_owned();
        options.policy = Policy::Disabled;
        options.max_inflight = 1;
        options.execution_profile.adapter = Provider::LlamaCpp;
        options.execution_profile.implementation_id = Some("llama-cpp".to_owned());
        let mut launch = options.execution_profile.options().cloned().unwrap();
        launch.parallel_slots = Some(1);
        options.execution_profile.set_local_options(launch).unwrap();
        config.models.push(options);
        let untrimmed = serde_json::to_vec(&config).unwrap().len();
        let remove = untrimmed.checked_sub(CONFIG_BYTES - 1).unwrap();
        let argument = config.models[count - 1]
            .execution_profile
            .args
            .last_mut()
            .unwrap();
        assert!(remove > 0 && remove < argument.len());
        argument.truncate(argument.len() - remove);
    }
    config.validate().unwrap();
    let mut bytes = serde_json::to_vec(&config).unwrap();
    bytes.push(b'\n');
    assert!(bytes.len() <= CONFIG_BYTES);
    if exact_boundary {
        assert_eq!(bytes.len(), CONFIG_BYTES);
    } else {
        assert!(bytes.len() > 15 * 1024 * 1024);
    }
    std::fs::write(&server.config_path, bytes).unwrap();
    (server, config)
}

fn error(response: (StatusCode, Value), status: StatusCode, code: &str) {
    assert_eq!(response.0, status, "{}", response.1);
    assert_eq!(response.1["error"]["code"], code);
}

async fn idle(server: &Server) {
    let catalog = server.get("juiced/catalog").await;
    for model in catalog["data"].as_array().unwrap() {
        assert_eq!(model["policy"], "disabled");
        assert_eq!(model["loaded"], false);
        assert!(model["pid"].is_null());
        assert!(model["process_lifecycle"].is_null());
    }
    let view = server.get("juiced/admission").await;
    assert_eq!(view["resources"]["reserved_bytes"], 0);
    assert_eq!(view["resources"]["active_requests"], 0);
    assert_eq!(view["resources"]["power"]["active"], 0);
    assert_eq!(view["resources"]["power"]["attempts"], 0);
    assert_eq!(view["startup"]["active"], 0);
    assert_eq!(view["startup"]["pending"], 0);
    assert_eq!(view["processes"], json!([]));
    assert_eq!(view["pending_preparations"], json!([]));
}

async fn durable_artifacts(server: &Server) -> Value {
    let mut value = server.get("juiced/artifacts").await;
    value.as_object_mut().unwrap().remove("view");
    value
}

#[tokio::test]
async fn oversized_managed_registration_preserves_readable_config_and_artifact_references_across_restart()
 {
    let source = Source::start().await;
    let (mut server, config) = prepare(15, false);
    server.start().await;
    let inventory = server.get("juiced/artifacts").await;
    let (status, started) = server
        .post(
            "juiced/artifacts/pull",
            json!({"expected_generation":inventory["generation"],"download":source.download()}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{started}");
    let job = started["id"].as_str().unwrap();
    server
        .wait_for(async || {
            let job = server.get(&format!("juiced/artifacts/jobs/{job}")).await;
            job["state"] == "complete" && job["writer_active"] == false
        })
        .await;
    let completed = server.get(&format!("juiced/artifacts/jobs/{job}")).await;
    let artifact = completed["artifact"].as_str().unwrap();
    let before = durable_artifacts(&server).await;
    let journal = tree(&server.directory.path().join("artifacts"));
    let disk = std::fs::read(&server.config_path).unwrap();
    let catalog = server.get("juiced/catalog").await;
    let mut registration = padding(&config.models[0], "managed.too-large");
    registration.artifact = Some(artifact.to_owned());
    registration.model_file = PathBuf::new();
    let request = serde_json::to_value(&registration).unwrap();
    assert!(serde_json::to_vec(&request).unwrap().len() < 2 * 1024 * 1024);
    let mut invalid = request.clone();
    invalid["policy"] = json!("hot");
    error(
        server.post("models/register", invalid).await,
        StatusCode::BAD_REQUEST,
        "registration_policy",
    );
    let mut duplicate = request.clone();
    duplicate["id"] = json!(config.models[0].id);
    error(
        server.post("models/register", duplicate).await,
        StatusCode::CONFLICT,
        "model_exists",
    );
    error(
        server.post("models/register", request).await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "configuration_too_large",
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert_eq!(server.get("juiced/catalog").await, catalog);
    assert_eq!(durable_artifacts(&server).await, before);
    assert_eq!(tree(&server.directory.path().join("artifacts")), journal);
    idle(&server).await;
    server.restart().await;
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert_eq!(server.get("juiced/catalog").await, catalog);
    assert_eq!(durable_artifacts(&server).await, before);
    assert_eq!(tree(&server.directory.path().join("artifacts")), journal);
    idle(&server).await;
}

#[tokio::test]
async fn exact_read_limit_policy_and_options_growth_refuse_publication_while_a_smaller_candidate_saves_and_restarts()
 {
    let (mut server, config) = prepare(16, true);
    server.start().await;
    let disk = std::fs::read(&server.config_path).unwrap();
    let catalog = server.get("juiced/catalog").await;
    let options = server.get("models/options.boundary/options").await;
    error(
        server
            .post(
                "juiced/policy",
                json!({"model":"padding.00","policy":"on-demand","expected_revision":1}),
            )
            .await,
        StatusCode::CONFLICT,
        "revision_conflict",
    );
    error(
        server
            .post(
                "juiced/policy",
                json!({"model":"missing","policy":"on-demand","expected_revision":0}),
            )
            .await,
        StatusCode::NOT_FOUND,
        "unknown_model",
    );
    let mut policy_candidate = config.clone();
    policy_candidate.generation = 1;
    policy_candidate.models[0].revision = 1;
    policy_candidate.models[0].policy = Policy::OnDemand;
    assert_eq!(
        serde_json::to_vec(&policy_candidate).unwrap().len() + 1,
        CONFIG_BYTES + 1
    );
    error(
        server
            .post(
                "juiced/policy",
                json!({"model":"padding.00","policy":"on-demand","expected_revision":0}),
            )
            .await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "configuration_too_large",
    );
    let too_large = json!({"expected_revision":0,"options":{"parallel_slots":1,"threads":65536}});
    error(
        server
            .post("models/options.boundary/options", too_large)
            .await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "configuration_too_large",
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), disk);
    assert_eq!(server.get("juiced/catalog").await, catalog);
    assert_eq!(server.get("models/options.boundary/options").await, options);
    idle(&server).await;
    let (status, changed) = server
        .post(
            "models/options.boundary/options",
            json!({"expected_revision":0,"options":{"parallel_slots":1,"threads":1}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{changed}");
    assert_eq!(changed["saved"]["threads"], 1);
    let saved = std::fs::read(&server.config_path).unwrap();
    assert_eq!(saved.len(), CONFIG_BYTES - 3);
    let persisted: Config = serde_json::from_slice(&saved).unwrap();
    assert_eq!(persisted.generation, 1);
    let selected = persisted
        .models
        .iter()
        .find(|model| model.id == "options.boundary")
        .unwrap();
    assert_eq!(selected.revision, 1);
    assert_eq!(
        selected.execution_profile.options().unwrap(),
        &LaunchOptions {
            threads: Some(1),
            parallel_slots: Some(1),
            ..LaunchOptions::default()
        }
    );
    server.restart().await;
    assert_eq!(std::fs::read(&server.config_path).unwrap(), saved);
    assert_eq!(server.get("models/options.boundary/options").await, changed);
    let catalog = server.get("juiced/catalog").await;
    error(
        server
            .post(
                "models/options.boundary/options",
                json!({"expected_revision":1,"options":{"parallel_slots":1,"threads":65536}}),
            )
            .await,
        StatusCode::PAYLOAD_TOO_LARGE,
        "configuration_too_large",
    );
    assert_eq!(std::fs::read(&server.config_path).unwrap(), saved);
    assert_eq!(server.get("juiced/catalog").await, catalog);
    assert_eq!(server.get("models/options.boundary/options").await, changed);
    idle(&server).await;
}
