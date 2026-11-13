use crate::{
    ApiError,
    auth::RequestContext,
    config::{Operation, Policy},
    extract::{ApiBytes, ApiQuery},
    media::{MAX_IMAGE_RESPONSE_BYTES, Txt2ImgRequest, validate_txt2img_response},
    supervisor::Manager,
};
use axum::{
    Extension, Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

const MAX_PROBE_BYTES: usize = 1024 * 1024;
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// The checkpoint chosen through `POST /sdapi/v1/options` lives only in memory; after a restart
/// clients fall back to the single enabled image model or name one again.
#[derive(Clone)]
struct Images {
    manager: Arc<Manager>,
    checkpoint: Arc<Mutex<Option<String>>>,
}

pub fn router(manager: Arc<Manager>) -> Router {
    Router::new()
        .route("/sdapi/v1/sd-models", get(sd_models))
        .route("/sdapi/v1/options", get(options).post(set_options))
        .route("/sdapi/v1/samplers", get(samplers))
        .route("/sdapi/v1/schedulers", get(schedulers))
        .route("/sdapi/v1/progress", get(progress))
        .route("/sdapi/v1/upscalers", get(empty))
        .route("/sdapi/v1/sd-vae", get(empty))
        .route("/sdapi/v1/latent-upscale-modes", get(empty))
        .route("/sdapi/v1/txt2img", post(txt2img))
        .route("/sdapi/v1/img2img", post(not_supported))
        .route("/sdapi/v1/interrupt", post(not_supported))
        .method_not_allowed_fallback(|| async {
            ApiError::new(
                StatusCode::METHOD_NOT_ALLOWED,
                "method_not_allowed",
                "Use a documented method for this endpoint.",
            )
        })
        .layer(DefaultBodyLimit::max(crate::admission::JSON_INGRESS_BYTES))
        .with_state(Images {
            manager,
            checkpoint: Arc::default(),
        })
}

fn reply(status: StatusCode, value: Value) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")], Json(value)).into_response()
}

fn conflict(code: &'static str, message: &'static str) -> ApiError {
    ApiError::new(StatusCode::CONFLICT, code, message)
}

impl Images {
    async fn candidates(&self) -> (Vec<String>, Vec<String>) {
        let catalog = self.manager.catalog().await;
        let usable = catalog
            .iter()
            .filter(|model| {
                model.policy != Policy::Disabled
                    && model.downloaded
                    && model.operations.contains(&Operation::ImageGeneration)
            })
            .map(|model| model.id.clone())
            .collect();
        (usable, catalog.into_iter().map(|model| model.id).collect())
    }

    async fn select(&self, requested: Option<String>) -> Result<String, ApiError> {
        let (usable, registered) = self.candidates().await;
        let requested = requested.or_else(|| {
            self.checkpoint
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone()
        });
        match requested {
            Some(model) if usable.contains(&model) => Ok(model),
            Some(model) if registered.contains(&model) => Err(conflict(
                "image_model_unavailable",
                "Select an enabled, downloaded image model.",
            )),
            Some(_) => Err(conflict("unknown_image_model", "Unknown image model.")),
            None => match <[String; 1]>::try_from(usable) {
                Ok([model]) => Ok(model),
                Err(usable) if usable.is_empty() => Err(conflict(
                    "image_model_required",
                    "Enable an image model first.",
                )),
                Err(_) => Err(conflict(
                    "image_model_ambiguous",
                    "Multiple image models are enabled; supply model or select sd_model_checkpoint.",
                )),
            },
        }
    }

    /// Reads one engine-reported list from the running worker; an unloaded worker reports
    /// nothing, and loading it here would make a read start inference.
    async fn probe(
        &self,
        model: &str,
        path: &str,
    ) -> Result<Option<(StatusCode, Bytes)>, ApiError> {
        let Some(base) = self.manager.running_base(model)? else {
            return Ok(None);
        };
        let unavailable = || {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "upstream_unavailable",
                "The image worker did not answer this probe.",
            )
        };
        let mut response = self
            .manager
            .client
            .get(format!("{base}{path}"))
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .map_err(|_| unavailable())?;
        let status = response.status();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if body.len() + chunk.len() > MAX_PROBE_BYTES {
                return Err(unavailable());
            }
            body.extend_from_slice(&chunk);
        }
        serde_json::from_slice::<Value>(&body).map_err(|_| unavailable())?;
        Ok(Some((
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            Bytes::from(body),
        )))
    }
}

#[derive(Deserialize)]
struct ProbeQuery {
    model: Option<String>,
}

async fn sd_models(State(images): State<Images>) -> Response {
    let models: Vec<Value> = images
        .candidates()
        .await
        .0
        .into_iter()
        .map(|name| {
            json!({"title": name, "model_name": name, "hash": null, "sha256": null, "filename": name, "config": null})
        })
        .collect();
    reply(StatusCode::OK, Value::Array(models))
}

async fn options(
    State(images): State<Images>,
    ApiQuery(query): ApiQuery<ProbeQuery>,
) -> Result<Response, ApiError> {
    let model = images.select(query.model).await?;
    Ok(reply(StatusCode::OK, json!({"sd_model_checkpoint": model})))
}

async fn set_options(
    State(images): State<Images>,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let fields = match serde_json::from_slice::<Value>(&body) {
        Ok(Value::Object(fields)) => fields,
        _ => {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_sdapi_request",
                "Invalid SD API request.",
            ));
        }
    };
    let Some(Value::String(checkpoint)) = fields
        .get("sd_model_checkpoint")
        .filter(|_| fields.len() == 1)
    else {
        return Err(conflict(
            "unsupported_option",
            "Only sd_model_checkpoint may be changed through this interface.",
        ));
    };
    let model = images.select(Some(checkpoint.clone())).await?;
    *images
        .checkpoint
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(model);
    Ok(reply(StatusCode::OK, Value::Null))
}

/// Before the worker runs, the engine's own list is unknown; `default` lets the worker apply
/// its configured sampler or scheduler.
async fn engine_list(
    images: Images,
    model: Option<String>,
    path: &str,
) -> Result<Response, ApiError> {
    let model = images.select(model).await?;
    Ok(match images.probe(&model, path).await? {
        Some((status, body)) => engine_reply(status, body),
        None if path.ends_with("samplers") => reply(
            StatusCode::OK,
            json!([{"name": "default", "aliases": [], "options": {}}]),
        ),
        None => reply(
            StatusCode::OK,
            json!([{"name": "default", "label": "Default"}]),
        ),
    })
}

fn engine_reply(status: StatusCode, body: Bytes) -> Response {
    (
        status,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        body,
    )
        .into_response()
}

async fn samplers(
    State(images): State<Images>,
    ApiQuery(query): ApiQuery<ProbeQuery>,
) -> Result<Response, ApiError> {
    engine_list(images, query.model, "/sdapi/v1/samplers").await
}

async fn schedulers(
    State(images): State<Images>,
    ApiQuery(query): ApiQuery<ProbeQuery>,
) -> Result<Response, ApiError> {
    engine_list(images, query.model, "/sdapi/v1/schedulers").await
}

async fn progress(
    State(images): State<Images>,
    ApiQuery(query): ApiQuery<ProbeQuery>,
) -> Result<Response, ApiError> {
    let model = images.select(query.model).await?;
    Ok(
        match images
            .probe(&model, "/sdapi/v1/progress?skip_current_image=true")
            .await?
        {
            Some((status, body)) => engine_reply(status, body),
            None => reply(
                StatusCode::OK,
                json!({"progress": 0.0, "eta_relative": 0, "state": {"job_count": 0, "job_no": 0}, "current_image": null, "textinfo": "Model is unloaded"}),
            ),
        },
    )
}

async fn empty() -> Response {
    reply(StatusCode::OK, json!([]))
}

async fn not_supported() -> Response {
    reply(
        StatusCode::NOT_IMPLEMENTED,
        json!({"error": "This operation is not supported by Juiced's SD API subset"}),
    )
}

async fn txt2img(
    State(images): State<Images>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let priority = crate::http::request_priority(&context, &headers)?;
    let request = Txt2ImgRequest::parse(&body)?;
    let model = images.select(request.model.clone()).await?;
    let (bytes, mut lease) = crate::http::media_exchange(
        &images.manager,
        &model,
        Operation::ImageGeneration,
        priority,
        "/sdapi/v1/txt2img",
        request.upstream(),
        MAX_IMAGE_RESPONSE_BYTES,
    )
    .await?;
    validate_txt2img_response(&bytes, request.batch)
        .inspect_err(|_| lease.finish("upstream_error", Some(200)))?;
    lease.finish("completed", Some(200));
    Ok(crate::http::media_reply("application/json", bytes))
}
