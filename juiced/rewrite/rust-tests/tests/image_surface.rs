mod support;

use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use support::{
    CLIENT_KEY, Server,
    speech::{self, Part},
};

const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    bytes.extend_from_slice(b"\0\0\0\x0dIHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(b"\x08\x06\0\0\0");
    bytes
}

fn image_worker(config: &mut Value, index: usize, id: &str) {
    let model = &mut config["models"][index];
    model["id"] = json!(id);
    model["operations"] = json!(["images/generations"]);
    model["provider"] = json!("stable-diffusion-cpp");
    model["max_inflight"] = json!(1);
    let events = model["args"][5].clone();
    model["args"] = json!([
        "--listen-port",
        "{port}",
        "--model",
        "{model}",
        "--events",
        events
    ]);
}

async fn image_server(record: Option<&std::path::Path>) -> Server {
    let mut server = Server::prepare(true, |config| image_worker(config, 0, "fixture.one"));
    if let Some(record) = record {
        server.set_environment("FIXTURE_LAUNCH_RECORD", record);
    }
    server.start().await;
    server
}

async fn sdapi(
    server: &Server,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    server.json(method, path, body, Some(CLIENT_KEY)).await
}

async fn form(server: &Server, path: &str, parts: &[Part<'_>]) -> (StatusCode, Value) {
    let (content_type, body) = speech::multipart(parts);
    let response = server
        .client
        .post(format!("{}{path}", server.base))
        .bearer_auth(CLIENT_KEY)
        .header("content-type", content_type)
        .body(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.json().await.unwrap())
}

fn decoded_png(encoded: &Value) -> bool {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(encoded.as_str().unwrap())
        .unwrap()
        .starts_with(PNG_SIGNATURE)
}

#[tokio::test]
async fn sillytavern_a1111_sequence_lists_selects_polls_and_generates() {
    let server = image_server(None).await;
    for path in [
        "/sdapi/v1/sd-models",
        "/sdapi/v1/options",
        "/sdapi/v1/samplers",
    ] {
        for key in [None, Some("invalid-key")] {
            let (status, _) = server.json(Method::GET, path, None, key).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        }
    }
    let (status, ping) = sdapi(&server, Method::GET, "/sdapi/v1/options", None).await;
    assert_eq!(status, StatusCode::OK, "{ping}");
    assert_eq!(ping, json!({"sd_model_checkpoint":"fixture.one"}));
    let (status, models) = sdapi(&server, Method::GET, "/sdapi/v1/sd-models", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        models,
        json!([{"title":"fixture.one","model_name":"fixture.one","hash":null,"sha256":null,"filename":"fixture.one","config":null}])
    );
    let (_, samplers) = sdapi(&server, Method::GET, "/sdapi/v1/samplers", None).await;
    assert_eq!(
        samplers,
        json!([{"name":"default","aliases":[],"options":{}}])
    );
    let (_, schedulers) = sdapi(&server, Method::GET, "/sdapi/v1/schedulers", None).await;
    assert_eq!(schedulers, json!([{"name":"default","label":"Default"}]));
    assert!(server.model("fixture.one").await["pid"].is_null());
    for path in [
        "/sdapi/v1/upscalers",
        "/sdapi/v1/sd-vae",
        "/sdapi/v1/latent-upscale-modes",
    ] {
        assert_eq!(
            sdapi(&server, Method::GET, path, None).await,
            (StatusCode::OK, json!([]))
        );
    }

    let (status, selected) = sdapi(
        &server,
        Method::POST,
        "/sdapi/v1/options",
        Some(json!({"sd_model_checkpoint":"fixture.one"})),
    )
    .await;
    assert_eq!((status, selected), (StatusCode::OK, Value::Null));
    let (status, idle) = sdapi(&server, Method::GET, "/sdapi/v1/progress", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(idle["progress"], 0.0);
    assert_eq!(idle["state"]["job_count"], 0);
    for (body, status, code) in [
        (
            json!({"sd_model_checkpoint":"missing"}),
            StatusCode::CONFLICT,
            "unknown_image_model",
        ),
        (
            json!({"sd_model_checkpoint":"fixture.two"}),
            StatusCode::CONFLICT,
            "image_model_unavailable",
        ),
        (
            json!({"sd_model_checkpoint":"fixture.one","samples_format":"png"}),
            StatusCode::CONFLICT,
            "unsupported_option",
        ),
        (json!([]), StatusCode::BAD_REQUEST, "invalid_sdapi_request"),
    ] {
        let (actual, error) = sdapi(&server, Method::POST, "/sdapi/v1/options", Some(body)).await;
        assert_eq!(
            (actual, error["error"]["code"].as_str()),
            (status, Some(code))
        );
    }

    let body = json!({
        "url":"http://juiced.example:8080","auth":"user:private-sillytavern-secret",
        "prompt":"a lighthouse","negative_prompt":"fog","sampler_name":"default","scheduler":"default",
        "steps":4,"cfg_scale":7,"width":512,"height":512,"restore_faces":false,"enable_hr":false,
        "hr_upscaler":"Latent","hr_scale":2,"hr_additional_modules":[],"denoising_strength":0.7,
        "hr_second_pass_steps":0,"override_settings":{"CLIP_stop_at_last_layers":1},
        "override_settings_restore_afterwards":true,"clip_skip":1,"save_images":true,"send_images":true,
        "do_not_save_grid":false,"do_not_save_samples":false
    });
    let (status, generated) = sdapi(
        &server,
        Method::POST,
        "/sdapi/v1/txt2img",
        Some(body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{generated}");
    assert!(decoded_png(&generated["images"][0]));
    let parameters = &generated["parameters"];
    assert_eq!(
        parameters,
        &json!({"prompt":"a lighthouse","negative_prompt":"fog","sampler_name":"default","scheduler":"default","steps":4,"cfg_scale":7.0,"width":512,"height":512,"clip_skip":1,"batch_size":1})
    );
    assert!(!generated.to_string().contains("private-sillytavern-secret"));
    let mut batch = body;
    batch["batch_size"] = json!(2);
    let (status, pair) = sdapi(&server, Method::POST, "/sdapi/v1/txt2img", Some(batch)).await;
    assert_eq!(status, StatusCode::OK, "{pair}");
    assert_eq!(pair["images"].as_array().unwrap().len(), 2);

    let (_, samplers) = sdapi(&server, Method::GET, "/sdapi/v1/samplers", None).await;
    assert_eq!(samplers[1]["name"], "euler_a");
    let (_, schedulers) = sdapi(&server, Method::GET, "/sdapi/v1/schedulers", None).await;
    assert_eq!(schedulers[1]["name"], "karras");
    let (_, progress) = sdapi(&server, Method::GET, "/sdapi/v1/progress", None).await;
    assert_eq!(progress["query"], json!({"skip_current_image":"true"}));

    for path in ["/sdapi/v1/img2img", "/sdapi/v1/interrupt"] {
        let response = server
            .request(Method::POST, path, Some(json!({})), Some(CLIENT_KEY))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(response.headers()["cache-control"], "no-store");
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"error":"This operation is not supported by Juiced's SD API subset"})
        );
    }
    for (body, status, code) in [
        (
            json!({"prompt":"x","alwayson_scripts":{"ADetailer":{"args":[]}}}),
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
        ),
        (
            json!({"prompt":"x","enable_hr":true}),
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
        ),
        (
            json!({"prompt":"x","batch_size":5}),
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
        ),
        (
            json!({"prompt":"x","model":"missing"}),
            StatusCode::CONFLICT,
            "unknown_image_model",
        ),
    ] {
        let (actual, error) = sdapi(&server, Method::POST, "/sdapi/v1/txt2img", Some(body)).await;
        assert_eq!(
            (actual, error["error"]["code"].as_str()),
            (status, Some(code)),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a1111_requires_a_selection_when_several_image_models_are_enabled() {
    let server = Server::configured(true, |config| {
        image_worker(config, 0, "fixture.one");
        image_worker(config, 1, "fixture.two");
    })
    .await;
    let (status, error) = sdapi(&server, Method::GET, "/sdapi/v1/options", None).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["error"]["code"], "image_model_ambiguous");
    let (_, models) = sdapi(&server, Method::GET, "/sdapi/v1/sd-models", None).await;
    assert_eq!(models.as_array().unwrap().len(), 2);
    let (status, _) = sdapi(
        &server,
        Method::POST,
        "/sdapi/v1/options",
        Some(json!({"sd_model_checkpoint":"fixture.two"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, current) = sdapi(&server, Method::GET, "/sdapi/v1/options", None).await;
    assert_eq!(current["sd_model_checkpoint"], "fixture.two");
    let (_, explicit) = sdapi(
        &server,
        Method::GET,
        "/sdapi/v1/options?model=fixture.one",
        None,
    )
    .await;
    assert_eq!(explicit["sd_model_checkpoint"], "fixture.one");
    let (status, generated) = sdapi(
        &server,
        Method::POST,
        "/sdapi/v1/txt2img",
        Some(json!({"prompt":"x","override_settings":{"sd_model_checkpoint":"fixture.one"}})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{generated}");
    assert!(!server.model("fixture.one").await["pid"].is_null());
    assert!(server.model("fixture.two").await["pid"].is_null());
}

#[tokio::test]
async fn openai_image_edits_and_variations_forward_multipart_to_the_edit_worker() {
    let server = image_server(None).await;
    let source = png(640, 480);
    let mask = png(640, 480);
    let (status, edited) = form(
        &server,
        "/v1/images/edits",
        &[
            Part::File("image", "source_image.png", "image/png", &source),
            Part::Text("model", "fixture.one"),
            Part::Text("prompt", "Add a red barn"),
            Part::File("mask", "mask.png", "image/png", &mask),
            Part::Text("steps", "6"),
            Part::Text("quality", "auto"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{edited}");
    assert!(decoded_png(&edited["data"][0]["b64_json"]));
    assert_eq!(edited["received"]["size"], "640x480");
    assert_eq!(edited["received"]["n"], "1");
    assert_eq!(edited["received"]["output_format"], "png");
    let prompt = edited["received"]["prompt"].as_str().unwrap();
    assert!(
        prompt.starts_with("Add a red barn <sd_cpp_extra_args>")
            && prompt.contains("\"sample_steps\":6"),
        "{prompt}"
    );
    let files = edited["files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files[0]["field"], "image[]");
    assert_eq!(files[0]["content_type"], "image/png");
    assert_eq!(files[0]["bytes"], source.len());
    assert_eq!(files[1]["field"], "mask");

    let large = png(4000, 3000);
    let (status, references) = form(
        &server,
        "/api/v1/images/edits",
        &[
            Part::Text("model", "fixture.one"),
            Part::Text("prompt", "Merge"),
            Part::File("image[]", "a.png", "image/png", &large),
            Part::File("image[]", "b.png", "image/png", &source),
            Part::Text("n", "2"),
            Part::Text("output_format", "jpeg"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{references}");
    assert_eq!(references["data"].as_array().unwrap().len(), 2);
    assert_eq!(references["received"]["size"], "2048x1536");
    assert_eq!(references["files"].as_array().unwrap().len(), 2);

    let (status, variation) = form(
        &server,
        "/v1/images/variations",
        &[
            Part::File("image", "source_image.png", "image/png", &source),
            Part::Text("model", "fixture.one"),
            Part::Text("size", "512x512"),
            Part::Text("n", "2"),
            Part::Text("response_format", "b64_json"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{variation}");
    assert_eq!(variation["data"].as_array().unwrap().len(), 2);
    assert_eq!(variation["received"]["prompt"], "variation");
    assert_eq!(variation["received"]["size"], "512x512");

    let image = || Part::File("image", "x.png", "image/png", &source);
    for (path, parts, status, code) in [
        (
            "/v1/images/edits",
            vec![
                Part::Text("model", "fixture.one"),
                Part::Text("prompt", "x"),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/edits",
            vec![image(), Part::Text("model", "fixture.one")],
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/edits",
            vec![image(), Part::Text("prompt", "x")],
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/edits",
            vec![
                image(),
                Part::Text("model", "fixture.one"),
                Part::Text("prompt", "x"),
                Part::Text("n", "11"),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/edits",
            vec![
                image(),
                Part::Text("model", "fixture.one"),
                Part::Text("prompt", "x"),
                Part::Text("steps", "a"),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/edits",
            vec![
                image(),
                Part::File("image[]", "y.png", "image/png", &source),
                Part::Text("model", "fixture.one"),
                Part::Text("prompt", "x"),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/edits",
            vec![
                Part::File("image", "x.gif", "image/gif", b"GIF89a"),
                Part::Text("model", "fixture.one"),
                Part::Text("prompt", "x"),
            ],
            StatusCode::BAD_REQUEST,
            "invalid_image_request",
        ),
        (
            "/v1/images/edits",
            vec![
                image(),
                Part::Text("model", "missing"),
                Part::Text("prompt", "x"),
            ],
            StatusCode::NOT_FOUND,
            "unknown_model",
        ),
        (
            "/v1/images/edits",
            vec![
                image(),
                Part::Text("model", "fixture.two"),
                Part::Text("prompt", "x"),
            ],
            StatusCode::NOT_IMPLEMENTED,
            "unsupported_operation",
        ),
        (
            "/v1/images/edits",
            vec![
                image(),
                Part::Text("model", "fixture.one"),
                Part::Text("prompt", "x"),
                Part::Text("output_format", "webp"),
            ],
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
        ),
        (
            "/v1/images/variations",
            vec![
                image(),
                Part::File("mask", "m.png", "image/png", &source),
                Part::Text("model", "fixture.one"),
            ],
            StatusCode::BAD_REQUEST,
            "unsupported_parameter",
        ),
    ] {
        let (actual, error) = form(&server, path, &parts).await;
        assert_eq!(
            (actual, error["error"]["code"].as_str()),
            (status, Some(code)),
            "{path}: {error}"
        );
    }
    let (status, error) = sdapi(
        &server,
        Method::POST,
        "/v1/images/edits",
        Some(json!({"model":"fixture.one","prompt":"x"})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        error["error"]["message"],
        "Request must be multipart/form-data."
    );
}

#[tokio::test]
async fn image_upscale_reports_the_native_validation_outcomes() {
    let server = image_server(None).await;
    for (body, status, message) in [
        (
            json!({}),
            StatusCode::BAD_REQUEST,
            "Missing 'image' field (base64 encoded)",
        ),
        (
            json!({"image":"AAAA"}),
            StatusCode::BAD_REQUEST,
            "Missing 'model' field",
        ),
        (
            json!({"image":"AAAA","model":"RealESRGAN-x4plus"}),
            StatusCode::NOT_FOUND,
            "Upscale model not found: RealESRGAN-x4plus",
        ),
        (
            json!({"image":"AAAA","model":"fixture.one"}),
            StatusCode::BAD_REQUEST,
            "Upscale model is not labeled 'upscaling': fixture.one",
        ),
    ] {
        let (actual, error) = sdapi(&server, Method::POST, "/v1/images/upscale", Some(body)).await;
        assert_eq!(
            (actual, error["error"]["message"].as_str()),
            (status, Some(message))
        );
    }
}

#[tokio::test]
async fn image_defaults_are_edited_through_model_options_and_reach_the_worker() {
    let record = tempfile::NamedTempFile::new().unwrap();
    let server = image_server(Some(record.path())).await;
    let view = server.get("models/fixture.one/options").await;
    assert_eq!(
        view["image_defaults"],
        json!({"steps":null,"cfg_scale":null,"width":null,"height":null,"sampling_method":null,"scheduler":null,"flow_shift":null,"seed":null})
    );
    let revision = view["revision"].as_u64().unwrap();
    for (body, status, code) in [
        (
            json!({"expected_revision":revision,"image_defaults":{"steps":0}}),
            StatusCode::BAD_REQUEST,
            "invalid_image_defaults",
        ),
        (
            json!({"expected_revision":revision,"image_defaults":{"ctx_size":4096}}),
            StatusCode::BAD_REQUEST,
            "invalid_image_defaults",
        ),
        (
            json!({"expected_revision":revision,"image_defaults":{},"options":{}}),
            StatusCode::BAD_REQUEST,
            "options_body",
        ),
        (
            json!({"expected_revision":revision + 7,"image_defaults":{"steps":4}}),
            StatusCode::CONFLICT,
            "revision_conflict",
        ),
    ] {
        let (actual, error) = server.post("models/fixture.one/options", body).await;
        assert_eq!(
            (actual, error["error"]["code"].as_str()),
            (status, Some(code)),
            "{error}"
        );
    }
    let chat = server.get("models/fixture.two/options").await;
    assert!(chat.get("image_defaults").is_none());
    let (status, error) = server
        .post(
            "models/fixture.two/options",
            json!({"expected_revision":chat["revision"],"image_defaults":{"steps":4}}),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("image_defaults_unsupported"))
    );

    let (status, saved) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":revision,"image_defaults":{"steps":6,"cfg_scale":1.5,"width":768,"height":512,"sampling_method":"euler","flow_shift":3}}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{saved}");
    assert_eq!(
        saved["image_defaults"],
        json!({"steps":6,"cfg_scale":1.5,"width":768,"height":512,"sampling_method":"euler","scheduler":null,"flow_shift":3.0,"seed":null})
    );
    assert!(saved["revision"].as_u64().unwrap() > revision);
    let (status, _) = sdapi(
        &server,
        Method::POST,
        "/sdapi/v1/txt2img",
        Some(json!({"prompt":"x"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let launches = std::fs::read_to_string(record.path()).unwrap();
    assert!(
        launches.contains(
            "--steps 6 --cfg-scale 1.5 -W 768 -H 512 --sampling-method euler --flow-shift 3"
        ),
        "{launches}"
    );

    let (status, error) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":saved["revision"],"image_defaults":{"steps":4}}),
        )
        .await;
    assert_eq!(
        (status, error["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("worker_in_use"))
    );
    let (status, reloaded) = server
        .post(
            "models/fixture.one/options",
            json!({"expected_revision":saved["revision"],"image_defaults":{"steps":4,"width":null},"apply":"reload"}),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{reloaded}");
    assert_eq!(reloaded["restarted"], true);
    assert_eq!(reloaded["image_defaults"]["steps"], 4);
    assert!(reloaded["image_defaults"]["width"].is_null());
    let launches = std::fs::read_to_string(record.path()).unwrap();
    assert!(
        launches
            .lines()
            .rfind(|line| line.starts_with("argv:"))
            .unwrap()
            .contains("--cfg-scale 1.5 -H 512 --sampling-method euler --flow-shift 3 --steps 4"),
        "{launches}"
    );
}
