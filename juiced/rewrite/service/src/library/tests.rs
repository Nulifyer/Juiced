use super::*;
use crate::{
    artifacts::{ArtifactLimits, ArtifactStore},
    storage::DirectoryLease,
};
use axum::{
    Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

const REVISION: &str = "0123456789abcdef0123456789abcdef01234567";
const SPLIT_REPO: &str = "fixture/Split-GGUF";
const SINGLE_REPO: &str = "fixture/Tiny-GGUF";

struct FakeFile {
    path: &'static str,
    bytes: Vec<u8>,
    lfs: bool,
}

struct Fake {
    repos: BTreeMap<&'static str, Vec<FakeFile>>,
    search_requests: AtomicUsize,
    info_requests: AtomicUsize,
    download_requests: AtomicUsize,
    mode: AtomicU8,
    held: std::sync::atomic::AtomicBool,
    release: tokio::sync::Notify,
    authorization: Mutex<Vec<(String, bool)>>,
    queries: Mutex<Vec<BTreeMap<String, String>>>,
}

const NORMAL: u8 = 0;
const SLOW: u8 = 1;
const MALFORMED: u8 = 2;
const OVERSIZED: u8 = 3;

struct Fixture {
    fake: Arc<Fake>,
    base: reqwest::Url,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn content(seed: u8, length: usize) -> Vec<u8> {
    (0..length)
        .map(|index| (index % 241) as u8 ^ seed)
        .collect()
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn record(fake: &Fake, headers: &HeaderMap, api: bool) {
    let authorization = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    fake.authorization
        .lock()
        .unwrap()
        .push((authorization, api));
}

async fn search(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    record(&fake, &headers, true);
    fake.search_requests.fetch_add(1, Ordering::AcqRel);
    fake.queries.lock().unwrap().push(query.clone());
    match fake.mode.load(Ordering::Acquire) {
        SLOW => tokio::time::sleep(Duration::from_secs(5)).await,
        MALFORMED => return "{\"not\":\"an array\"".into_response(),
        OVERSIZED => return "x".repeat(8192).into_response(),
        _ => {}
    }
    let items = json!([
        {"id": SPLIT_REPO, "downloads": 900, "likes": 12, "pipeline_tag": "text-generation", "tags": ["gguf", "license:mit"]},
        {"id": SINGLE_REPO, "likes": 3, "gated": "manual"},
        {"id": "not a repo id", "downloads": 1},
        {"id": "fixture/Embed-GGUF", "pipeline_tag": "feature-extraction", "downloads": 5},
    ]);
    let mut response = axum::Json(items).into_response();
    if !query.contains_key("cursor") {
        response.headers_mut().insert(
            header::LINK,
            "<https://hub.invalid/api/models?search=x&cursor=eyJwYWdlIjoyfQ%3D%3D>; rel=\"next\""
                .parse()
                .unwrap(),
        );
    }
    response
}

async fn info_response(fake: &Fake, repo: &str, revision: Option<&str>) -> Response {
    fake.info_requests.fetch_add(1, Ordering::AcqRel);
    match fake.mode.load(Ordering::Acquire) {
        SLOW => tokio::time::sleep(Duration::from_secs(5)).await,
        MALFORMED => return "{\"id\":".into_response(),
        _ => {}
    }
    if revision.is_some_and(|revision| revision != REVISION) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(files) = fake.repos.get(repo) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let siblings: Vec<Value> = files
        .iter()
        .map(|file| {
            if file.lfs {
                json!({"rfilename": file.path, "size": file.bytes.len(),
                       "lfs": {"sha256": digest(&file.bytes).to_ascii_uppercase(), "size": file.bytes.len(), "pointerSize": 134}})
            } else {
                json!({"rfilename": file.path, "size": file.bytes.len()})
            }
        })
        .chain([json!({"rfilename": "README.md", "size": 12}), json!({"rfilename": 7})])
        .collect();
    axum::Json(json!({
        "id": repo, "sha": REVISION, "downloads": 900, "likes": 12,
        "cardData": {"license": "apache-2.0"}, "gated": false, "siblings": siblings,
    }))
    .into_response()
}

async fn info(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<BTreeMap<String, String>>,
) -> Response {
    record(&fake, &headers, true);
    assert_eq!(query.get("blobs").map(String::as_str), Some("true"));
    info_response(&fake, &format!("{owner}/{name}"), None).await
}

async fn revision_info(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Path((owner, name, revision)): Path<(String, String, String)>,
) -> Response {
    record(&fake, &headers, true);
    info_response(&fake, &format!("{owner}/{name}"), Some(&revision)).await
}

async fn resolve(
    State(fake): State<Arc<Fake>>,
    headers: HeaderMap,
    Path((owner, name, revision, file)): Path<(String, String, String, String)>,
) -> Response {
    record(&fake, &headers, false);
    fake.download_requests.fetch_add(1, Ordering::AcqRel);
    loop {
        let released = fake.release.notified();
        tokio::pin!(released);
        released.as_mut().enable();
        if !fake.held.load(Ordering::Acquire) {
            break;
        }
        released.await;
    }
    if revision != REVISION {
        return StatusCode::NOT_FOUND.into_response();
    }
    let repo = format!("{owner}/{name}");
    let Some(found) = fake
        .repos
        .get(repo.as_str())
        .and_then(|files| files.iter().find(|known| known.path == file))
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [(header::ETAG, format!("\"{}\"", digest(&found.bytes)))],
        found.bytes.clone(),
    )
        .into_response()
}

fn repos() -> BTreeMap<&'static str, Vec<FakeFile>> {
    let file = |path, seed, length| FakeFile {
        path,
        bytes: content(seed, length),
        lfs: true,
    };
    BTreeMap::from([
        (
            SPLIT_REPO,
            vec![
                file("Split-Q4_K_M-00001-of-00003.gguf", 1, 40_000),
                file("Split-Q4_K_M-00002-of-00003.gguf", 2, 30_000),
                file("Split-Q4_K_M-00003-of-00003.gguf", 3, 20_000),
                file("Split-Q8_0-00001-of-00002.gguf", 4, 10_000),
                file("Split-IQ4_NL.gguf", 5, 9_000),
                file("mmproj-Split-F16.gguf", 6, 1_000),
                FakeFile {
                    path: "Split-F16.gguf",
                    bytes: content(7, 500),
                    lfs: false,
                },
            ],
        ),
        (SINGLE_REPO, vec![file("Tiny-Q4_K_M.gguf", 8, 50_000)]),
        (
            "fixture/Embed-GGUF",
            vec![file("Embed-Q8_0.gguf", 9, 2_000)],
        ),
    ])
}

async fn fixture() -> Fixture {
    let fake = Arc::new(Fake {
        repos: repos(),
        search_requests: AtomicUsize::new(0),
        info_requests: AtomicUsize::new(0),
        download_requests: AtomicUsize::new(0),
        mode: AtomicU8::new(NORMAL),
        held: std::sync::atomic::AtomicBool::new(false),
        release: tokio::sync::Notify::new(),
        authorization: Mutex::new(Vec::new()),
        queries: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/api/models", get(search))
        .route("/api/models/{owner}/{name}", get(info))
        .route(
            "/api/models/{owner}/{name}/revision/{revision}",
            get(revision_info),
        )
        .route("/{owner}/{name}/resolve/{revision}/{*file}", get(resolve))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let task = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    Fixture { fake, base, task }
}

fn limits() -> HubLimits {
    HubLimits {
        request_timeout: Duration::from_millis(800),
        search_deadline: Duration::from_secs(2),
        search_bytes: 4096,
        ..HubLimits::default()
    }
}

fn library(fixture: &Fixture, policy: SourcePolicy, token: Option<&str>) -> Arc<Library> {
    Arc::new(Library::new(
        Some(fixture.base.clone()),
        token.and_then(|token| Token::new(token.to_owned())),
        policy,
        limits(),
        Duration::from_millis(20),
    ))
}

fn store(directory: &tempfile::TempDir, policy: SourcePolicy) -> Arc<ArtifactStore> {
    ArtifactStore::open(
        DirectoryLease::acquire(directory.path().join("artifacts")).unwrap(),
        ArtifactLimits::default(),
        policy,
    )
    .unwrap()
}

fn query(q: &str) -> SearchQuery {
    SearchQuery {
        q: Some(q.to_owned()),
        task: None,
        limit: None,
        cursor: None,
    }
}

fn entry<'a>(result: &'a Value, id: &str) -> &'a Value {
    result["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
        .unwrap_or_else(|| panic!("search result {id} missing from {result}"))
}

fn quant<'a>(entry: &'a Value, file: &str) -> &'a Value {
    entry["quants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|quant| quant["file"] == file)
        .unwrap_or_else(|| panic!("quant {file} missing"))
}

fn hold(fixture: &Fixture, held: bool) {
    fixture.fake.held.store(held, Ordering::Release);
    fixture.fake.release.notify_waiters();
}

async fn wait_for_pull(library: &Library, id: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let view = library.pull_view(id).unwrap();
        if matches!(view["state"].as_str(), Some("complete" | "failed")) {
            return view;
        }
        assert!(
            Instant::now() < deadline,
            "library pull did not settle: {view}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[test]
fn quant_labels_follow_published_gguf_and_ggml_names() {
    for (name, label) in [
        ("Qwen3-8B-Q4_K_M.gguf", Some("Q4_K_M")),
        ("qwen3-8b-q4_k_m.gguf", Some("Q4_K_M")),
        ("Qwen3.5-4B-UD-Q4_K_XL.gguf", Some("UD-Q4_K_XL")),
        ("model-IQ4_NL.gguf", Some("IQ4_NL")),
        ("model-IQ2_XXS.gguf", Some("IQ2_XXS")),
        ("model-IQ3_XS-00001-of-00002.gguf", Some("IQ3_XS")),
        ("model.Q8_0.gguf", Some("Q8_0")),
        ("model-Q4_0_4_4.gguf", Some("Q4_0")),
        ("model-F16.gguf", Some("F16")),
        ("model-bf16.gguf", Some("BF16")),
        ("gpt-oss-20b-mxfp4.gguf", Some("MXFP4")),
        ("Qwen3-Coder-Next-MXFP4_MOE.gguf", Some("MXFP4_MOE")),
        ("Bonsai-8B-Q1_0.gguf", Some("Q1_0")),
        ("ggml-tiny.en-q5_1.bin", Some("Q5_1")),
        ("UD-Q4_K_XL/Model-00001-of-00002.gguf", Some("UD-Q4_K_XL")),
        ("ggml-base.bin", None),
        ("Qwen3-8B.gguf", None),
        ("model-Q4_K_Mx.gguf", Some("Q4_K")),
    ] {
        let parsed = super::quant::quant_label(super::quant::basename(name))
            .or_else(|| super::quant::quant_label(name));
        assert_eq!(parsed.as_deref(), label, "{name}");
    }
}

#[test]
fn split_parts_group_into_ordered_complete_variants_and_skip_companions() {
    let file = |path: &str, bytes: Option<u64>| super::quant::FileMeta {
        path: path.into(),
        bytes,
        sha256: Some("a".repeat(64)),
    };
    let variants = super::quant::group(&[
        file("Q4_K_M/Model-Q4_K_M-00002-of-00003.gguf", Some(2)),
        file("Q4_K_M/Model-Q4_K_M-00001-of-00003.gguf", Some(1)),
        file("Q4_K_M/Model-Q4_K_M-00003-of-00003.gguf", Some(3)),
        file("Model-Q8_0-00001-of-00002.gguf", Some(5)),
        file("Model-IQ4_NL.gguf", None),
        file("mmproj-Model-F16.gguf", Some(1)),
        file("mtp-Model.gguf", Some(1)),
        file("README.md", Some(1)),
    ]);
    assert_eq!(variants.len(), 3);
    let split = variants.iter().find(|variant| variant.parts == 3).unwrap();
    assert_eq!(split.primary, "Q4_K_M/Model-Q4_K_M-00001-of-00003.gguf");
    assert_eq!(split.label.as_deref(), Some("Q4_K_M"));
    assert!(split.complete && split.verifiable());
    assert_eq!(split.bytes(), Some(6));
    let partial = variants.iter().find(|variant| variant.parts == 2).unwrap();
    assert!(!partial.complete && !partial.verifiable());
    let single = variants
        .iter()
        .find(|variant| variant.label.as_deref() == Some("IQ4_NL"))
        .unwrap();
    assert_eq!(single.bytes(), None);
    assert!(!single.verifiable());
    assert_eq!(super::quant::split_part("a-00000-of-00002.gguf"), None);
    assert_eq!(
        super::quant::split_part("modèle-00001-of-00002.gguf").map(|part| part.1),
        Some(1)
    );
    assert_eq!(super::quant::split_part("é.gguf"), None);
    assert_eq!(super::quant::split_part("ééé"), None);
    assert_eq!(
        super::quant::quant_label("modèle-Q4_K_M.gguf").as_deref(),
        Some("Q4_K_M")
    );
    assert_eq!(super::quant::stem("modèle-Q4_K_M.gguf"), "modèle");
    assert_eq!(super::quant::split_part("a-00003-of-00002.gguf"), None);
    assert_eq!(super::quant::stem("ggml-tiny.en-q8_0.bin"), "ggml-tiny.en");
    assert_eq!(
        super::quant::stem("ggml-large-v3-turbo-q5_0.bin"),
        "ggml-large-v3-turbo"
    );
    assert_eq!(super::quant::stem("ggml-base.bin"), "ggml-base");
}

#[test]
fn compiled_catalog_passes_schema_check_and_rejects_invalid_entries() {
    let compiled = super::catalog::catalog();
    assert!(compiled.models.len() >= 20);
    for task in [Task::Chat, Task::Embeddings, Task::Transcription] {
        assert!(compiled.models.iter().any(|model| model.task == task));
    }
    let original: Value = serde_json::from_str(include_str!("catalog.json")).unwrap();
    for (field, value) in [
        ("id", json!("Qwen3-0.6b")),
        ("repo", json!("unsloth")),
        ("repo", json!("../escape")),
        ("recommended_quant", json!("Q4_K_Mx")),
        ("recommended_quant", Value::Null),
        ("license", json!("Apache 2.0")),
        ("task", json!("vision")),
    ] {
        let mut changed = original.clone();
        changed["models"][0][field] = value.clone();
        let parsed: Result<super::catalog::Catalog, _> = serde_json::from_value(changed);
        assert!(
            parsed.map_or(true, |catalog| catalog.validate().is_err()),
            "{field}={value} passed the catalog schema"
        );
    }
    let mut duplicate = original.clone();
    let first = duplicate["models"][0].clone();
    duplicate["models"].as_array_mut().unwrap().push(first);
    let catalog: super::catalog::Catalog = serde_json::from_value(duplicate).unwrap();
    assert!(catalog.validate().is_err());
    let mut whisper = original.clone();
    let index = whisper["models"]
        .as_array()
        .unwrap()
        .iter()
        .position(|model| model["variant_stem"].is_string())
        .unwrap();
    whisper["models"][index]["variant_stem"] = json!("ggml-other");
    let catalog: super::catalog::Catalog = serde_json::from_value(whisper).unwrap();
    assert!(catalog.validate().is_err());
}

#[test]
fn hub_metadata_parsing_keeps_unknown_values_null_and_extracts_lfs_digests() {
    let page = super::hub::parse_search(
        br#"[{"id":"a/b","likes":2},{"id":"bad"},{"downloads":3},{"id":"a/b"},{"id":"c/d","downloads":7,"gated":"auto","tags":["license:mit"]}]"#,
        20,
        None,
    )
    .unwrap();
    assert_eq!(page.repos.len(), 2);
    assert_eq!(page.skipped, 3);
    assert_eq!(page.repos[0].downloads, None);
    assert_eq!(page.repos[0].gated, None);
    assert_eq!(page.repos[1].downloads, Some(7));
    assert_eq!(page.repos[1].gated, Some(true));
    assert_eq!(page.repos[1].license.as_deref(), Some("mit"));
    assert_eq!(
        super::hub::parse_search(b"{}", 20, None).err(),
        Some(HubError::Malformed)
    );
    assert_eq!(
        super::hub::parse_search(b"[", 20, None).err(),
        Some(HubError::Malformed)
    );
    let sha = "AB".repeat(32);
    let body = json!({
        "id": "a/b", "sha": REVISION,
        "siblings": [
            {"rfilename": "m-Q4_K_M.gguf", "size": 10, "lfs": {"sha256": sha, "size": 10}},
            {"rfilename": "m-Q8_0.gguf", "size": 11, "lfs": {"sha256": sha, "size": 12}},
            {"rfilename": "m-F16.gguf", "size": 13},
            {"rfilename": "../m-Q2_K.gguf", "size": 1, "lfs": {"sha256": sha, "size": 1}},
            {"rfilename": "m-Q5_K_M.gguf", "lfs": {"sha256": "short", "size": 0}},
            {"rfilename": "config.json", "size": 3},
            {"rfilename": "m-Q6_K.gguf", "size": 2, "lfs": {"sha256": sha, "size": 2}},
        ],
    });
    let info = super::hub::parse_info(body.to_string().as_bytes(), 4).unwrap();
    assert_eq!(info.revision.as_deref(), Some(REVISION));
    assert_eq!(info.files.len(), 4);
    assert!(info.files_truncated);
    assert_eq!(info.files[0].bytes, Some(10));
    assert_eq!(info.files[0].sha256, Some("ab".repeat(32)));
    assert_eq!(
        (info.files[1].bytes, info.files[1].sha256.as_ref()),
        (None, None)
    );
    assert_eq!(
        (info.files[2].bytes, info.files[2].sha256.as_ref()),
        (Some(13), None)
    );
    assert_eq!(
        (info.files[3].bytes, info.files[3].sha256.as_ref()),
        (None, None)
    );
    let unpinned = json!({"id":"a/b","sha":"main","siblings":[]});
    assert_eq!(
        super::hub::parse_info(unpinned.to_string().as_bytes(), 4)
            .unwrap()
            .revision,
        None
    );
    assert_eq!(
        super::hub::parse_info(br#"{"id":"a/b"}"#, 4).err(),
        Some(HubError::Malformed)
    );
}

#[tokio::test]
async fn ttl_cache_expires_and_bounds_entries_and_bytes() {
    let cache = super::hub::TtlCache::new(Duration::from_millis(60), 2, 64);
    cache.insert("a".into(), Arc::new(1), 10);
    cache.insert("b".into(), Arc::new(2), 10);
    assert_eq!(cache.get("a").as_deref(), Some(&1));
    cache.insert("c".into(), Arc::new(3), 10);
    assert_eq!(cache.len(), 2);
    assert!(cache.get("a").is_none());
    cache.insert("large".into(), Arc::new(4), 128);
    assert!(cache.get("large").is_none());
    cache.insert("d".into(), Arc::new(5), 60);
    assert_eq!(cache.len(), 1);
    assert_eq!(cache.get("d").as_deref(), Some(&5));
    tokio::time::sleep(Duration::from_millis(80)).await;
    assert!(cache.get("d").is_none());
    assert_eq!(cache.len(), 0);
}

#[tokio::test]
async fn search_groups_catalog_and_hub_results_and_caches_upstream_reads() {
    let fixture = fixture().await;
    let library = library(
        &fixture,
        SourcePolicy::AllowLoopbackHttp,
        Some("hub-secret-token"),
    );
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, SourcePolicy::AllowLoopbackHttp);
    let result = library
        .search(Some(store.clone()), query("fixture"))
        .await
        .unwrap();
    assert_eq!(result["sources"]["huggingface"]["status"], "ok");
    assert_eq!(result["sources"]["huggingface"]["cached"], false);
    assert_eq!(result["sources"]["huggingface"]["skipped"], 1);
    assert_eq!(result["sources"]["catalog"]["count"], 0);
    assert_eq!(result["page"]["next_cursor"], "eyJwYWdlIjoyfQ==");
    assert_eq!(result["artifact_generation"], store.generation());
    let split = entry(&result, SPLIT_REPO);
    assert_eq!(split["source"], "huggingface");
    assert_eq!(split["task"], "chat");
    assert_eq!(split["downloads"], 900);
    assert_eq!(split["license"], "mit");
    assert_eq!(split["revision"], REVISION);
    assert_eq!(split["files_truncated"], false);
    assert_eq!(split["quants"].as_array().unwrap().len(), 4);
    let three = quant(split, "Split-Q4_K_M-00001-of-00003.gguf");
    assert_eq!(three["label"], "Q4_K_M");
    assert_eq!(three["parts"], 3);
    assert_eq!(three["bytes"], 90_000);
    assert_eq!(three["sha256"], Value::Null);
    assert_eq!(three["pullable"], true);
    assert_eq!(three["local"]["present"], false);
    assert_eq!(three["files"][1]["sha256"], digest(&content(2, 30_000)));
    let partial = quant(split, "Split-Q8_0-00001-of-00002.gguf");
    assert_eq!(
        (partial["complete"].clone(), partial["pullable"].clone()),
        (json!(false), json!(false))
    );
    let unverifiable = quant(split, "Split-F16.gguf");
    assert_eq!(unverifiable["sha256"], Value::Null);
    assert_eq!(unverifiable["pullable"], false);
    let single = entry(&result, SINGLE_REPO);
    assert_eq!(single["downloads"], Value::Null);
    assert_eq!(single["gated"], true);
    assert_eq!(single["task"], Value::Null);
    assert_eq!(single["license"], "apache-2.0");
    let searches = fixture.fake.search_requests.load(Ordering::Acquire);
    let infos = fixture.fake.info_requests.load(Ordering::Acquire);
    let again = library
        .search(Some(store.clone()), query("fixture"))
        .await
        .unwrap();
    assert_eq!(again["sources"]["huggingface"]["cached"], true);
    assert_eq!(
        fixture.fake.search_requests.load(Ordering::Acquire),
        searches
    );
    assert_eq!(fixture.fake.info_requests.load(Ordering::Acquire), infos);
    let embeddings = library
        .search(
            Some(store.clone()),
            SearchQuery {
                task: Some(Task::Embeddings),
                ..query("embed")
            },
        )
        .await
        .unwrap();
    let ids: Vec<&str> = embeddings["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["id"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"fixture/Embed-GGUF"));
    assert!(!ids.contains(&SPLIT_REPO) && !ids.contains(&SINGLE_REPO));
    assert!(ids.contains(&"nomic-embed-text-v1"));
    let catalog = entry(&embeddings, "nomic-embed-text-v1");
    assert_eq!(catalog["source"], "catalog");
    assert_eq!(catalog["metadata"], "unavailable");
    assert_eq!(catalog["metadata_error"], "huggingface_not_found");
    assert_eq!(catalog["quants"], Value::Null);
    assert_eq!(catalog["recommended_quant"], "Q4_K_S");
    let next = library
        .search(
            Some(store.clone()),
            SearchQuery {
                cursor: Some("eyJwYWdlIjoyfQ==".into()),
                ..query("fixture")
            },
        )
        .await
        .unwrap();
    assert_eq!(next["page"]["next_cursor"], Value::Null);
    assert_eq!(
        fixture.fake.queries.lock().unwrap().last().unwrap()["cursor"],
        "eyJwYWdlIjoyfQ=="
    );
    let observed = fixture.fake.authorization.lock().unwrap().clone();
    assert!(!observed.is_empty());
    assert!(
        observed
            .iter()
            .all(|(authorization, api)| *api && authorization == "Bearer hub-secret-token")
    );
    assert!(!result.to_string().contains("hub-secret-token"));
    for invalid in [
        SearchQuery {
            limit: Some(0),
            ..query("x")
        },
        SearchQuery {
            limit: Some(21),
            ..query("x")
        },
        SearchQuery {
            cursor: Some("bad cursor".into()),
            ..query("x")
        },
        query(&"x".repeat(129)),
        query("bad\u{7}"),
    ] {
        let error = library.search(None, invalid).await.unwrap_err();
        assert_eq!(error.code, "invalid_library_query");
    }
}

#[tokio::test]
async fn bounded_upstream_failures_keep_the_catalog_available() {
    let fixture = fixture().await;
    let library = library(&fixture, SourcePolicy::AllowLoopbackHttp, None);
    for (mode, code) in [
        (SLOW, "huggingface_timeout"),
        (MALFORMED, "huggingface_malformed"),
        (OVERSIZED, "huggingface_response_too_large"),
    ] {
        fixture.fake.mode.store(mode, Ordering::Release);
        let started = Instant::now();
        let result = library
            .search(None, query(&format!("qwen3 {mode}")))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert_eq!(result["sources"]["huggingface"]["status"], "unavailable");
        assert_eq!(result["sources"]["huggingface"]["error"], code);
        assert_eq!(result["sources"]["local"]["status"], "unavailable");
    }
    fixture.fake.mode.store(MALFORMED, Ordering::Release);
    let result = library.search(None, query("qwen3")).await.unwrap();
    let catalog = entry(&result, "qwen3-0.6b");
    assert_eq!(catalog["source"], "catalog");
    assert_eq!(catalog["metadata_error"], "huggingface_malformed");
    assert_eq!(catalog["license"], "apache-2.0");
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, SourcePolicy::AllowLoopbackHttp);
    let error = library
        .pull(
            store.clone(),
            PullRequest {
                repo: SINGLE_REPO.into(),
                file: Some("Tiny-Q4_K_M.gguf".into()),
                files: None,
                revision: None,
                expected_generation: store.generation(),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.code),
        (StatusCode::BAD_GATEWAY, "huggingface_malformed")
    );
    fixture.fake.mode.store(SLOW, Ordering::Release);
    let started = Instant::now();
    let error = library
        .pull(
            store.clone(),
            PullRequest {
                repo: SINGLE_REPO.into(),
                file: Some("Tiny-Q4_K_M.gguf".into()),
                files: None,
                revision: Some(REVISION.into()),
                expected_generation: store.generation(),
            },
        )
        .await
        .unwrap_err();
    assert!(started.elapsed() < Duration::from_secs(4));
    assert_eq!(
        (error.status, error.code),
        (StatusCode::GATEWAY_TIMEOUT, "huggingface_timeout")
    );
    assert_eq!(fixture.fake.download_requests.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn offline_policy_serves_catalog_and_local_files_without_network() {
    let fixture = fixture().await;
    let online = library(&fixture, SourcePolicy::AllowLoopbackHttp, None);
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, SourcePolicy::AllowLoopbackHttp);
    let started = online
        .pull(
            store.clone(),
            PullRequest {
                repo: SINGLE_REPO.into(),
                file: Some("Tiny-Q4_K_M.gguf".into()),
                files: None,
                revision: None,
                expected_generation: store.generation(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        wait_for_pull(&online, started["pull"].as_str().unwrap()).await["state"],
        "complete"
    );
    let requests = (
        fixture.fake.search_requests.load(Ordering::Acquire),
        fixture.fake.info_requests.load(Ordering::Acquire),
    );
    let offline = library(&fixture, SourcePolicy::Offline, None);
    let result = offline
        .search(Some(store.clone()), query("tiny"))
        .await
        .unwrap();
    assert_eq!(result["offline"], true);
    assert_eq!(result["sources"]["huggingface"]["status"], "offline");
    assert_eq!(
        result["sources"]["huggingface"]["error"],
        "huggingface_offline"
    );
    let local = entry(&result, SINGLE_REPO);
    assert_eq!(local["source"], "local");
    assert_eq!(local["metadata"], "local");
    assert_eq!(local["revision"], REVISION);
    let tiny = quant(local, "Tiny-Q4_K_M.gguf");
    assert_eq!(tiny["local"]["present"], true);
    assert_eq!(tiny["pullable"], false);
    assert_eq!(tiny["sha256"], digest(&content(8, 50_000)));
    let catalog = offline
        .search(Some(store.clone()), query("whisper tiny"))
        .await
        .unwrap();
    let whisper = entry(&catalog, "whisper-tiny");
    assert_eq!(whisper["metadata_error"], "huggingface_offline");
    assert_eq!(whisper["recommended_file"], "ggml-tiny.bin");
    assert_eq!(
        (
            fixture.fake.search_requests.load(Ordering::Acquire),
            fixture.fake.info_requests.load(Ordering::Acquire),
        ),
        requests
    );
    let offline_directory = tempfile::tempdir().unwrap();
    let offline_store = self::store(&offline_directory, SourcePolicy::Offline);
    let error = online
        .pull(
            offline_store.clone(),
            PullRequest {
                repo: SINGLE_REPO.into(),
                file: Some("Tiny-Q4_K_M.gguf".into()),
                files: None,
                revision: None,
                expected_generation: 0,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (error.status, error.code),
        (StatusCode::FORBIDDEN, "artifact_offline")
    );
    let blocked = library(&fixture, SourcePolicy::PublicHttps, None);
    let result = blocked.search(None, query("qwen3")).await.unwrap();
    assert_eq!(result["sources"]["huggingface"]["status"], "blocked");
    assert_eq!(
        result["sources"]["huggingface"]["error"],
        "huggingface_source_policy"
    );
}

#[tokio::test]
async fn split_pull_pins_revision_queues_parts_and_publishes_one_set() {
    let fixture = fixture().await;
    let library = library(
        &fixture,
        SourcePolicy::AllowLoopbackHttp,
        Some("hub-secret-token"),
    );
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, SourcePolicy::AllowLoopbackHttp);
    hold(&fixture, true);
    let stale = library
        .pull(
            store.clone(),
            PullRequest {
                repo: SPLIT_REPO.into(),
                file: Some("Split-Q4_K_M-00002-of-00003.gguf".into()),
                files: None,
                revision: None,
                expected_generation: store.generation() + 1,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(stale.code, "artifact_revision_conflict");
    let started = library
        .pull(
            store.clone(),
            PullRequest {
                repo: SPLIT_REPO.into(),
                file: Some("Split-Q4_K_M-00002-of-00003.gguf".into()),
                files: None,
                revision: None,
                expected_generation: store.generation(),
            },
        )
        .await
        .unwrap();
    assert_eq!(started["revision"], REVISION);
    assert_eq!(started["label"], "Q4_K_M");
    assert_eq!(started["file"], "Split-Q4_K_M-00001-of-00003.gguf");
    assert_eq!(started["provenance"]["repo"], SPLIT_REPO);
    assert_eq!(started["files"].as_array().unwrap().len(), 3);
    assert_eq!(started["files"][2]["state"], "queued");
    assert_eq!(started["jobs"].as_array().unwrap().len(), 2);
    for (index, part) in started["files"].as_array().unwrap().iter().enumerate() {
        assert_eq!(
            part["source_url"],
            format!(
                "{}{SPLIT_REPO}/resolve/{REVISION}/Split-Q4_K_M-0000{}-of-00003.gguf",
                fixture.base,
                index + 1
            )
        );
    }
    let set = started["set"].as_str().unwrap().to_owned();
    assert_eq!(started["set_state"], "pending");
    let id = started["pull"].as_str().unwrap().to_owned();
    let repeated = library
        .pull(
            store.clone(),
            PullRequest {
                repo: SPLIT_REPO.into(),
                file: None,
                files: Some(vec!["Split-Q4_K_M-00003-of-00003.gguf".into()]),
                revision: Some(REVISION.into()),
                expected_generation: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(repeated["pull"], id);
    assert_eq!(repeated["reused_pull"], true);
    hold(&fixture, false);
    let finished = wait_for_pull(&library, &id).await;
    assert_eq!(finished["state"], "complete", "{finished}");
    assert_eq!(finished["set_state"], "published");
    let published = store.artifact_set(&set).unwrap();
    assert_eq!(
        published.specification.primary,
        "Split-Q4_K_M-00001-of-00003.gguf"
    );
    assert_eq!(published.specification.origin.importer_id, "huggingface");
    assert_eq!(published.specification.origin.source_revision, REVISION);
    let roles: Vec<_> = published
        .specification
        .members
        .iter()
        .map(|member| (member.name.as_str(), member.role))
        .collect();
    assert_eq!(
        roles,
        [
            ("Split-Q4_K_M-00001-of-00003.gguf", ArtifactRole::Model),
            (
                "Split-Q4_K_M-00002-of-00003.gguf",
                ArtifactRole::WeightShard
            ),
            (
                "Split-Q4_K_M-00003-of-00003.gguf",
                ArtifactRole::WeightShard
            ),
        ]
    );
    assert!(published.members_present);
    let downloads = fixture.fake.download_requests.load(Ordering::Acquire);
    assert_eq!(downloads, 3);
    let observed = fixture.fake.authorization.lock().unwrap().clone();
    assert!(
        observed
            .iter()
            .filter(|(_, api)| !*api)
            .all(|(authorization, _)| authorization.is_empty())
    );
    let result = library
        .search(Some(store.clone()), query("fixture"))
        .await
        .unwrap();
    let three = quant(
        entry(&result, SPLIT_REPO),
        "Split-Q4_K_M-00001-of-00003.gguf",
    );
    assert_eq!(three["local"]["present"], true);
    assert_eq!(three["local"]["present_files"], 3);
    assert_eq!(three["local"]["set"], set);
    assert_eq!(three["jobs"], json!([]));
    let again = library
        .pull(
            store.clone(),
            PullRequest {
                repo: SPLIT_REPO.into(),
                file: Some("Split-Q4_K_M-00001-of-00003.gguf".into()),
                files: None,
                revision: None,
                expected_generation: 0,
            },
        )
        .await
        .unwrap();
    assert!(
        again["files"]
            .as_array()
            .unwrap()
            .iter()
            .all(|part| part["state"] == "present")
    );
    assert_eq!(
        wait_for_pull(&library, again["pull"].as_str().unwrap()).await["set_state"],
        "published"
    );
    assert_eq!(
        fixture.fake.download_requests.load(Ordering::Acquire),
        downloads
    );
}

#[tokio::test]
async fn active_downloads_are_marked_and_reused_by_later_pulls() {
    let fixture = fixture().await;
    let library = library(&fixture, SourcePolicy::AllowLoopbackHttp, None);
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, SourcePolicy::AllowLoopbackHttp);
    hold(&fixture, true);
    let job = store
        .start_download(
            DownloadRequest {
                source_url: format!(
                    "{}{SINGLE_REPO}/resolve/{REVISION}/Tiny-Q4_K_M.gguf",
                    fixture.base
                ),
                expected_sha256: digest(&content(8, 50_000)),
                expected_bytes: Some(50_000),
                maximum_bytes: 50_000,
            },
            store.generation(),
        )
        .unwrap();
    let result = library
        .search(Some(store.clone()), query("fixture"))
        .await
        .unwrap();
    let tiny = quant(entry(&result, SINGLE_REPO), "Tiny-Q4_K_M.gguf");
    assert_eq!(tiny["jobs"][0]["id"], job.id);
    assert_eq!(tiny["jobs"][0]["writer_active"], true);
    assert_eq!(tiny["local"]["present"], false);
    let reused = library
        .pull(
            store.clone(),
            PullRequest {
                repo: SINGLE_REPO.into(),
                file: Some("Tiny-Q4_K_M.gguf".into()),
                files: None,
                revision: None,
                expected_generation: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(reused["files"][0]["job"], job.id);
    assert_eq!(reused["files"][0]["reused_job"], true);
    assert_eq!(reused["set"], Value::Null);
    hold(&fixture, false);
    let finished = wait_for_pull(&library, reused["pull"].as_str().unwrap()).await;
    assert_eq!(finished["state"], "complete");
    assert_eq!(finished["files"][0]["state"], "present");
    assert_eq!(fixture.fake.download_requests.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn pull_rejects_ambiguous_incomplete_unverifiable_and_unknown_files() {
    let fixture = fixture().await;
    let library = library(&fixture, SourcePolicy::AllowLoopbackHttp, None);
    let directory = tempfile::tempdir().unwrap();
    let store = store(&directory, SourcePolicy::AllowLoopbackHttp);
    let request =
        |file: Option<&str>, files: Option<Vec<&str>>, revision: Option<&str>| PullRequest {
            repo: SPLIT_REPO.into(),
            file: file.map(str::to_owned),
            files: files.map(|files| files.into_iter().map(str::to_owned).collect()),
            revision: revision.map(str::to_owned),
            expected_generation: 0,
        };
    for (request, status, code) in [
        (
            request(None, None, None),
            StatusCode::BAD_REQUEST,
            "invalid_library_pull",
        ),
        (
            request(Some("../x.gguf"), None, None),
            StatusCode::BAD_REQUEST,
            "invalid_library_pull",
        ),
        (
            request(Some("a.gguf"), None, Some("main")),
            StatusCode::BAD_REQUEST,
            "invalid_library_pull",
        ),
        (
            request(
                None,
                Some(vec![
                    "Split-IQ4_NL.gguf",
                    "Split-Q4_K_M-00001-of-00003.gguf",
                ]),
                None,
            ),
            StatusCode::BAD_REQUEST,
            "invalid_library_pull",
        ),
        (
            request(Some("Split-Q8_0-00001-of-00002.gguf"), None, None),
            StatusCode::UNPROCESSABLE_ENTITY,
            "library_split_incomplete",
        ),
        (
            request(Some("Split-F16.gguf"), None, None),
            StatusCode::UNPROCESSABLE_ENTITY,
            "library_file_unverifiable",
        ),
        (
            request(Some("mmproj-Split-F16.gguf"), None, None),
            StatusCode::NOT_FOUND,
            "library_file_not_found",
        ),
        (
            request(Some("Absent-Q4_0.gguf"), None, None),
            StatusCode::NOT_FOUND,
            "library_file_not_found",
        ),
        (
            request(
                Some("Split-IQ4_NL.gguf"),
                None,
                Some("fedcba9876543210fedcba9876543210fedcba98"),
            ),
            StatusCode::NOT_FOUND,
            "library_repo_not_found",
        ),
    ] {
        let error = library.pull(store.clone(), request).await.unwrap_err();
        assert_eq!((error.status, error.code), (status, code));
    }
    let mut missing = request(Some("x.gguf"), None, None);
    missing.repo = "fixture/Missing".into();
    let error = library.pull(store.clone(), missing).await.unwrap_err();
    assert_eq!(error.code, "library_repo_not_found");
    assert_eq!(fixture.fake.download_requests.load(Ordering::Acquire), 0);
    assert_eq!(store.generation(), 0);
    assert_eq!(
        library
            .pull_view("00000000000000000000000000000000")
            .unwrap_err()
            .code,
        "library_pull_not_found"
    );
}
