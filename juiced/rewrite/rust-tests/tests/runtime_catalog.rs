mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use support::{PREFIXES, Server};

fn contract() -> Value {
    let mut seed: Value =
        serde_json::from_str(include_str!("../../catalog-design/inventory.json")).unwrap();
    for migration in seed["native_recipe_migrations"].as_array_mut().unwrap() {
        migration
            .as_object_mut()
            .unwrap()
            .entry("profile_overrides")
            .or_insert_with(|| json!([]));
    }
    seed
}
fn files(root: &Path) -> BTreeMap<PathBuf, Option<Vec<u8>>> {
    fn visit(root: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Option<Vec<u8>>>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            assert!(!kind.is_symlink());
            if kind.is_dir() {
                files.insert(path.strip_prefix(root).unwrap().to_path_buf(), None);
                visit(root, &path, files);
            } else {
                files.insert(
                    path.strip_prefix(root).unwrap().to_path_buf(),
                    Some(std::fs::read(path).unwrap()),
                );
            }
        }
    }
    let mut snapshot = BTreeMap::new();
    visit(root, root, &mut snapshot);
    snapshot
}
async fn resolve(server: &Server, recipe: &str, backend: &str) -> (StatusCode, Value) {
    let response = server
        .client
        .get(format!("{}/v1/juiced/runtimes/resolve", server.base))
        .bearer_auth(server.read_key().unwrap())
        .query(&[("recipe", recipe), ("backend", backend)])
        .send()
        .await
        .unwrap();
    (response.status(), response.json().await.unwrap())
}
fn blockers(resolution: &Value) -> BTreeSet<&str> {
    resolution["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|blocker| blocker["code"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn catalog_preserves_reviewed_definition_and_native_row_contract_as_stable_read_only_bytes() {
    let server = Server::new(true).await;
    let expected = contract();
    let disk = files(server.config_path.parent().unwrap());
    let state = server.get("juiced/state").await;
    let catalog = server.get("juiced/catalog").await;
    let requests = server.get("juiced/requests").await;
    let admission = server.get("juiced/admission").await;
    let mut original = None;
    for prefix in PREFIXES {
        let response = server
            .request(
                Method::GET,
                &format!("{prefix}/juiced/runtimes"),
                None,
                server.read_key(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert!(
            response.headers()["content-type"]
                .to_str()
                .unwrap()
                .contains("application/json")
        );
        let bytes = response.bytes().await.unwrap();
        if let Some(original) = &original {
            assert_eq!(&bytes, original);
        } else {
            original = Some(bytes);
        }
    }
    let projection: Value = serde_json::from_slice(original.as_ref().unwrap()).unwrap();
    assert_eq!(projection["schema_version"], 1);
    assert_eq!(projection["execution_enabled"], false);
    assert_eq!(projection["qualification_receipts"], json!([]));
    assert_eq!(projection["native_reference"]["recipe_count"], 18);
    assert_eq!(projection["native_reference"]["support_row_count"], 44);
    for field in [
        "native_reference",
        "native_recipe_migrations",
        "model_family_examples",
        "priority_definitions",
        "candidate_backend_semantics",
        "purpose",
        "reviewed_at",
    ] {
        assert_eq!(projection[field], expected[field], "{field}");
    }
    assert_eq!(projection["compute_paths"], expected["backends"]);
    assert_eq!(projection["discovery_tasks"], expected["tasks"]);
    let definitions = projection["definitions"].as_array().unwrap();
    assert_eq!(definitions.len(), 69);
    let mut ids = BTreeSet::new();
    for (definition, source) in definitions
        .iter()
        .zip(expected["entries"].as_array().unwrap())
    {
        let mut fields = definition.as_object().unwrap().clone();
        let adapter = fields.remove("adapter").unwrap();
        assert_eq!(Value::Object(fields), *source);
        let id = definition["id"].as_str().unwrap();
        assert!(ids.insert(id));
        assert_eq!(
            adapter["status"],
            if matches!(id, "llama-cpp" | "whisper-cpp") {
                "partial"
            } else {
                "planned"
            }
        );
        assert!(adapter["scope"].is_string());
    }
    for identity in [
        "futo-transcribe-cpp",
        "kokoros",
        "parakeet-redux",
        "llama-cpp",
        "llama-xdna-hybrid",
        "cactus-small",
        "cactus-engine",
    ] {
        assert!(ids.contains(identity), "{identity}");
    }
    assert_eq!(server.get("juiced/state").await, state);
    assert_eq!(server.get("juiced/catalog").await, catalog);
    assert_eq!(server.get("juiced/requests").await, requests);
    assert_eq!(server.get("juiced/admission").await, admission);
    assert_eq!(files(server.config_path.parent().unwrap()), disk);
}

#[tokio::test]
async fn every_exact_native_recipe_backend_resolves_to_its_preserved_implementation_and_support_rows()
 {
    let server = Server::new(true).await;
    let expected = contract();
    let identities: BTreeMap<&str, &str> = [
        ("acestep", "acestep-cpp"),
        ("asr4all", "futo-transcribe-cpp"),
        ("ds4", "dwarfstar4"),
        ("flm", "fastflowlm"),
        ("kokoro", "kokoros"),
        ("llamacpp", "llama-cpp"),
        ("llamacpp-hrx", "llama-hrx"),
        ("moonshine", "moonshine-voice"),
        ("onnxruntime", "onnxruntime"),
        ("openmoss", "openmoss-cpp"),
        ("parakeet", "parakeet-redux"),
        ("ryzenai-llm", "ryzenai-oga"),
        ("sd-cpp", "stable-diffusion-cpp"),
        ("thenoise", "thenoise"),
        ("thinksound", "thinksound-cpp"),
        ("trellis", "trellis-cpp"),
        ("vllm", "vllm"),
        ("whispercpp", "whisper-cpp"),
    ]
    .into_iter()
    .collect();
    let mut recipes = BTreeSet::new();
    let mut row_count = 0;
    for migration in expected["native_recipe_migrations"].as_array().unwrap() {
        let recipe = migration["legacy_recipe"].as_str().unwrap();
        assert!(recipes.insert(recipe));
        for backend in migration["native_backend_labels"].as_array().unwrap() {
            let backend = backend.as_str().unwrap();
            let (status, result) = resolve(&server, recipe, backend).await;
            assert_eq!(status, StatusCode::OK, "{recipe}/{backend}: {result}");
            let identity = if recipe == "llamacpp" && backend == "hybrid-rocm" {
                "llama-xdna-hybrid"
            } else {
                identities[recipe]
            };
            assert_eq!(result["legacy_recipe"], recipe);
            assert_eq!(result["legacy_backend"], backend);
            assert_eq!(result["implementation_id"], identity);
            assert_eq!(result["execution_enabled"], false);
            assert_eq!(result["qualification_receipts"], json!([]));
            assert_eq!(result["migration_rule"], migration["migration_rule"]);
            let rows: Vec<Value> = migration["native_support_rows"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["backend"] == backend)
                .cloned()
                .collect();
            row_count += rows.len();
            assert!(!rows.is_empty());
            assert_eq!(result["native_support_rows"], json!(rows));
            assert!(blockers(&result).contains("bundle_identity_required"));
            assert!(blockers(&result).contains("device_identity_required"));
        }
    }
    assert_eq!(recipes.len(), 18);
    assert_eq!(row_count, 44);
    assert_eq!(
        server.get("juiced/state").await["resources"]["reserved_bytes"],
        0
    );
    assert_eq!(server.get("juiced/requests").await["data"], json!([]));
}

#[tokio::test]
async fn vocabulary_mapping_preserves_hybrid_fork_provenance_npu_and_native_device_restrictions() {
    let server = Server::new(true).await;
    for (recipe, backend, identity, blocker) in [
        (
            "llamacpp",
            "system",
            "llama-cpp",
            "installation_provenance_unresolved",
        ),
        (
            "flm",
            "npu",
            "fastflowlm",
            "recipe_scoped_npu_stack_required",
        ),
        (
            "ryzenai-llm",
            "npu",
            "ryzenai-oga",
            "recipe_scoped_npu_stack_required",
        ),
        (
            "whispercpp",
            "npu",
            "whisper-cpp",
            "recipe_scoped_npu_stack_required",
        ),
        (
            "llamacpp",
            "hybrid-rocm",
            "llama-xdna-hybrid",
            "hybrid_placement_required",
        ),
    ] {
        let (status, result) = resolve(&server, recipe, backend).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["implementation_id"], identity);
        assert_eq!(result["compute_path_id"], Value::Null);
        assert!(blockers(&result).contains(blocker));
        assert_eq!(result["execution_enabled"], false);
        if backend == "hybrid-rocm" {
            assert_eq!(result["native_support_rows"][0]["os"], json!(["linux"]));
            assert_eq!(result["native_support_rows"][0]["tier"], "experimental");
            assert_eq!(
                result["native_support_rows"][0]["device_declaration"],
                "amd_gpu (gfx1150); amd_npu (XDNA2)"
            );
            assert!(result["override_reason"].is_string());
            assert!(blockers(&result).contains("adapter_not_implemented"));
        }
        if recipe == "ryzenai-llm" {
            assert_eq!(result["native_support_rows"][0]["os"], json!(["windows"]));
        }
    }
    let (status, rocm) = resolve(&server, "llamacpp", "rocm").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rocm["implementation_id"], "llama-cpp");
    assert_eq!(rocm["compute_path_id"], "hip");
    assert_eq!(rocm["legacy_backend"], "rocm");
    assert_eq!(rocm["native_support_rows"][0]["backend"], "rocm");
    assert_eq!(rocm["execution_enabled"], false);
    assert_eq!(rocm["qualification_receipts"], json!([]));
}

#[tokio::test]
async fn malformed_and_nonexact_runtime_selections_reject_without_mutating_model_or_store_state() {
    let server = Server::new(true).await;
    let disk = files(server.config_path.parent().unwrap());
    let state = server.get("juiced/state").await;
    let admission = server.get("juiced/admission").await;
    for (recipe, backend, status, code) in [
        (
            "llama-cpp",
            "cpu",
            StatusCode::NOT_FOUND,
            "unknown_legacy_recipe",
        ),
        (
            "LLAMACPP",
            "cpu",
            StatusCode::NOT_FOUND,
            "unknown_legacy_recipe",
        ),
        (
            "llamacpp",
            "hip",
            StatusCode::BAD_REQUEST,
            "unknown_legacy_backend",
        ),
        (
            "llamacpp",
            "CPU",
            StatusCode::BAD_REQUEST,
            "unknown_legacy_backend",
        ),
        (
            "llamacpp",
            " cpu",
            StatusCode::BAD_REQUEST,
            "unknown_legacy_backend",
        ),
        (
            "llamacpp",
            "sycl",
            StatusCode::BAD_REQUEST,
            "unknown_legacy_backend",
        ),
        (
            "llamacpp",
            "npu",
            StatusCode::BAD_REQUEST,
            "unknown_legacy_backend",
        ),
        (
            "kokoro",
            "cuda",
            StatusCode::BAD_REQUEST,
            "unknown_legacy_backend",
        ),
    ] {
        let (actual, error) = resolve(&server, recipe, backend).await;
        assert_eq!(actual, status);
        assert_eq!(error["error"]["code"], code);
    }
    for (recipe, backend) in [
        ("x".repeat(257), "cpu".to_owned()),
        ("llamacpp".to_owned(), "x".repeat(257)),
        ("llama\ncpp".to_owned(), "cpu".to_owned()),
        ("llamacpp".to_owned(), "cpu\0".to_owned()),
    ] {
        let (status, error) = resolve(&server, &recipe, &backend).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["error"]["code"], "invalid_runtime_selection");
    }
    for query in [
        "",
        "?recipe=llamacpp",
        "?backend=cpu",
        "?recipe=llamacpp&backend=cpu&unknown=1",
        "?recipe=llamacpp&recipe=flm&backend=cpu",
    ] {
        let (status, error) = server
            .json(
                Method::GET,
                &format!("/v1/juiced/runtimes/resolve{query}"),
                None,
                server.read_key(),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(error["error"]["message"].is_string());
    }
    assert_eq!(server.get("juiced/state").await, state);
    assert_eq!(server.get("juiced/admission").await, admission);
    assert_eq!(server.get("juiced/requests").await["data"], json!([]));
    assert_eq!(files(server.config_path.parent().unwrap()), disk);
}
