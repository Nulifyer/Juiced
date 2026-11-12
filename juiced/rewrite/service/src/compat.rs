use crate::{
    ApiError,
    auth::RequestContext,
    config::Operation,
    extract::ApiBytes,
    http::{Endpoint, automatic_session, forward_request, request_session},
    protocols::{self, CodecError, McpDispatch, OllamaOperation, ProtocolRequest, SseEvent},
    supervisor::Manager,
};
use axum::{
    Extension, Json, Router,
    body::{Body, BodyDataStream, Bytes, to_bytes},
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::{StreamExt, stream};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::watch;

const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_STREAM_CHUNK: usize = 1024 * 1024;
const MAX_PENDING_MCP: usize = 128;

pub const ENDPOINTS: &[Endpoint] = &[
    Endpoint {
        method: "POST",
        path: "/v1/messages",
        authority: "inference",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/v1/messages/count_tokens",
        authority: "inference",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/api/chat",
        authority: "inference",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/api/generate",
        authority: "inference",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/api/embed",
        authority: "inference",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/api/embeddings",
        authority: "inference",
        prefixes: &[""],
    },
    Endpoint {
        method: "GET",
        path: "/api/tags",
        authority: "read",
        prefixes: &[""],
    },
    Endpoint {
        method: "GET",
        path: "/api/ps",
        authority: "read",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/api/show",
        authority: "read",
        prefixes: &[""],
    },
    Endpoint {
        method: "GET",
        path: "/api/version",
        authority: "read",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/mcp",
        authority: "inference",
        prefixes: &[""],
    },
    Endpoint {
        method: "POST",
        path: "/sdapi/v1/txt2img",
        authority: "inference",
        prefixes: &[""],
    },
];

#[derive(Clone)]
struct Gateway {
    manager: Arc<Manager>,
    pending: Arc<PendingMcp>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct CancelKey {
    principal: &'static str,
    epoch: Arc<str>,
    session: Option<String>,
    id: String,
}

#[derive(Default)]
struct PendingMcp {
    requests: Mutex<HashMap<CancelKey, watch::Sender<bool>>>,
}

struct PendingRequest {
    key: CancelKey,
    pending: Arc<PendingMcp>,
}

impl Drop for PendingRequest {
    fn drop(&mut self) {
        self.pending
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&self.key);
    }
}

impl PendingMcp {
    fn register(
        self: &Arc<Self>,
        key: CancelKey,
    ) -> Result<(PendingRequest, watch::Receiver<bool>), CodecError> {
        let mut pending = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if pending.contains_key(&key) {
            return Err(codec(
                409,
                "duplicate_request",
                "This request ID is already active in the caller's scope.",
            ));
        }
        if pending.len() >= MAX_PENDING_MCP {
            return Err(codec(
                429,
                "request_slots",
                "The MCP gateway has no available request slots.",
            ));
        }
        let (sender, receiver) = watch::channel(false);
        pending.insert(key.clone(), sender);
        Ok((
            PendingRequest {
                key,
                pending: self.clone(),
            },
            receiver,
        ))
    }

    fn cancel(&self, key: &CancelKey) {
        if let Some(sender) = self
            .requests
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(key)
        {
            let _ = sender.send(true);
        }
    }
}

pub fn router(manager: Arc<Manager>) -> Router {
    Router::new()
        .route("/v1/messages", post(messages))
        .route("/v1/messages/count_tokens", post(count_tokens))
        .route("/api/chat", post(ollama_chat))
        .route("/api/generate", post(ollama_generate))
        .route("/api/embed", post(ollama_embed))
        .route("/api/embeddings", post(ollama_legacy_embed))
        .route("/api/tags", get(tags))
        .route("/api/ps", get(ps))
        .route("/api/show", post(show))
        .route("/api/version", get(version))
        .route("/mcp", post(mcp))
        .route("/sdapi/v1/txt2img", post(txt2img))
        .method_not_allowed_fallback(method_not_allowed)
        .layer(DefaultBodyLimit::max(MAX_RESPONSE_BYTES))
        .with_state(Gateway {
            manager,
            pending: Arc::new(PendingMcp::default()),
        })
}

fn codec(status: u16, code: &'static str, message: impl Into<String>) -> CodecError {
    CodecError {
        status,
        code,
        message: message.into(),
    }
}
fn from_api(error: ApiError) -> CodecError {
    codec(error.status.as_u16(), error.code, error.message)
}
fn json_response(status: u16, value: Value) -> Response {
    let mut response = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        Json(value),
    )
        .into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}
fn protocol_error(error: CodecError, anthropic: bool) -> Response {
    json_response(
        error.status,
        if anthropic {
            protocols::anthropic_error(&error)
        } else {
            protocols::ollama_error(&error)
        },
    )
}
pub(crate) fn rejection(path: &str, error: ApiError) -> Response {
    if path == "/mcp" {
        return json_response(
            error.status.as_u16(),
            protocols::mcp_error(Value::Null, -32001, &error.message),
        );
    }
    if !path.starts_with("/sdapi/") && ENDPOINTS.iter().any(|endpoint| endpoint.path == path) {
        return protocol_error(from_api(error), path.starts_with("/v1/messages"));
    }
    error.into_response()
}

async fn txt2img(
    State(gateway): State<Gateway>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    ApiBytes(body): ApiBytes,
) -> Result<Response, ApiError> {
    let priority = crate::http::request_priority(&context, &headers)?;
    let request = crate::media::Txt2ImgRequest::parse(&body)?;
    let model = match request.model.clone() {
        Some(model) => model,
        None => {
            let candidates: Vec<String> = gateway
                .manager
                .catalog()
                .await
                .into_iter()
                .filter(|model| {
                    model.policy != crate::config::Policy::Disabled
                        && model.downloaded
                        && model.operations.contains(&Operation::ImageGeneration)
                })
                .map(|model| model.id)
                .collect();
            let [model] = <[String; 1]>::try_from(candidates).map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "image_model_required",
                    "Name the image model; exactly one enabled image model is needed to omit it.",
                )
            })?;
            model
        }
    };
    let (bytes, mut lease) = crate::http::media_exchange(
        &gateway.manager,
        &model,
        Operation::ImageGeneration,
        priority,
        "/sdapi/v1/txt2img",
        request.upstream(),
        crate::media::MAX_IMAGE_RESPONSE_BYTES,
    )
    .await?;
    crate::media::validate_txt2img_response(&bytes)
        .inspect_err(|_| lease.finish("upstream_error", Some(200)))?;
    lease.finish("completed", Some(200));
    Ok(crate::http::media_reply("application/json", bytes))
}

fn parse_body(headers: &HeaderMap, body: Result<ApiBytes, ApiError>) -> Result<Value, CodecError> {
    let ApiBytes(body) = body.map_err(from_api)?;
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return Err(codec(415, "invalid_content_type", "Send application/json."));
    }
    serde_json::from_slice(&body).map_err(|_| codec(400, "invalid_request", "Send valid JSON."))
}
fn operation(request: &ProtocolRequest) -> Result<Operation, CodecError> {
    match request.operation {
        "chat/completions" => Ok(Operation::Chat),
        "completions" => Ok(Operation::Completion),
        "embeddings" => Ok(Operation::Embeddings),
        "messages/count_tokens" => Ok(Operation::TokenCount),
        _ => Err(codec(
            400,
            "unsupported_operation",
            "The protocol cannot route this operation.",
        )),
    }
}
async fn forward(
    state: &Gateway,
    headers: &HeaderMap,
    request: &ProtocolRequest,
    context: &RequestContext,
) -> Result<Response, CodecError> {
    request_session(headers).map_err(from_api)?;
    let automatic = automatic_session(headers).map_err(from_api)?;
    let body = serde_json::to_vec(&request.body)
        .map_err(|_| codec(400, "invalid_request", "The request cannot be encoded."))?;
    // A successful HTTP transfer cannot establish semantic validity for converted protocols.
    forward_request(
        state.manager.clone(),
        Bytes::from(body),
        operation(request)?,
        Some((context, None, "gateway")),
        automatic,
    )
    .await
    .map_err(from_api)
}
async fn collect(response: Response) -> Result<Value, CodecError> {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
        .await
        .map_err(|_| {
            codec(
                502,
                "invalid_upstream",
                "The worker response was interrupted or exceeded the size limit.",
            )
        })?;
    if !status.is_success() {
        return Err(codec(
            status.as_u16(),
            "worker_rejected",
            "The selected model service rejected the request.",
        ));
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| {
        codec(
            502,
            "invalid_upstream",
            "The worker did not return valid JSON.",
        )
    })?;
    if value.get("error").is_some() {
        return Err(codec(
            502,
            "worker_error",
            "The selected model service returned an error.",
        ));
    }
    Ok(value)
}

async fn messages(
    State(state): State<Gateway>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    body: Result<ApiBytes, ApiError>,
) -> Response {
    anthropic(state, context, headers, body, false).await
}
async fn count_tokens(
    State(state): State<Gateway>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    body: Result<ApiBytes, ApiError>,
) -> Response {
    anthropic(state, context, headers, body, true).await
}
async fn anthropic(
    state: Gateway,
    context: RequestContext,
    headers: HeaderMap,
    body: Result<ApiBytes, ApiError>,
    count: bool,
) -> Response {
    let result = async {
        let input = parse_body(&headers, body)?;
        let request = protocols::anthropic_request(input, count)?;
        let response = forward(&state, &headers, &request, &context).await?;
        if request.stream && response.status().is_success() {
            return Ok(stream_response(
                response,
                Encoder::Anthropic(protocols::AnthropicStream::new(
                    request.body["model"].as_str().unwrap_or_default(),
                )),
            ));
        }
        let value = collect(response).await?;
        let output = if count {
            protocols::anthropic_token_count(&value)?
        } else {
            let mut output = protocols::anthropic_response(&value)?;
            output["model"] = request.body["model"].clone();
            output
        };
        Ok::<_, CodecError>(json_response(200, output))
    }
    .await;
    result.unwrap_or_else(|error| protocol_error(error, true))
}

macro_rules! ollama_handler {
    ($name:ident, $operation:ident) => {
        async fn $name(
            State(state): State<Gateway>,
            Extension(context): Extension<RequestContext>,
            headers: HeaderMap,
            body: Result<ApiBytes, ApiError>,
        ) -> Response {
            ollama(state, context, headers, body, OllamaOperation::$operation).await
        }
    };
}
ollama_handler!(ollama_chat, Chat);
ollama_handler!(ollama_generate, Generate);
ollama_handler!(ollama_embed, Embed);
ollama_handler!(ollama_legacy_embed, LegacyEmbed);

async fn ollama(
    state: Gateway,
    context: RequestContext,
    headers: HeaderMap,
    body: Result<ApiBytes, ApiError>,
    operation: OllamaOperation,
) -> Response {
    let result = async {
        let mut input = parse_body(&headers, body)?;
        let keep_alive = input
            .as_object_mut()
            .and_then(|input| input.remove("keep_alive"))
            .filter(|value| !value.is_null())
            .map(|value| {
                crate::supervisor::parse_keep_alive(&value).ok_or_else(|| {
                    codec(
                        400,
                        "invalid_request",
                        "keep_alive must be seconds or a duration such as 5m.",
                    )
                })
            })
            .transpose()?;
        let request = protocols::ollama_request(input, operation)?;
        if let (Some(keep_alive), Some(model)) = (keep_alive, request.body["model"].as_str()) {
            state
                .manager
                .set_keep_alive(model, keep_alive)
                .map_err(from_api)?;
        }
        let response = forward(&state, &headers, &request, &context).await?;
        let model = request.body["model"].as_str().unwrap_or_default();
        let created_at = utc_now();
        if request.stream && response.status().is_success() {
            return Ok(stream_response(
                response,
                Encoder::Ollama(protocols::OllamaStream::new(model, created_at, operation)?),
            ));
        }
        let value = collect(response).await?;
        Ok::<_, CodecError>(json_response(
            200,
            protocols::ollama_response(&value, operation, model, &created_at)?,
        ))
    }
    .await;
    result.unwrap_or_else(|error| protocol_error(error, false))
}
async fn catalog(state: &Gateway) -> Vec<Value> {
    let models = state.manager.catalog().await;
    let aliases = state.manager.aliased_views(&models);
    models
        .into_iter()
        .filter_map(|model| serde_json::to_value(model).ok())
        .chain(aliases.into_iter().filter_map(|(alias, model)| {
            let mut value = serde_json::to_value(model).ok()?;
            value["aliased_to"] = value["id"].take();
            value["id"] = Value::String(alias);
            Some(value)
        }))
        .collect()
}
async fn tags(State(state): State<Gateway>) -> Response {
    json_response(200, protocols::ollama_models(&catalog(&state).await, false))
}
async fn ps(State(state): State<Gateway>) -> Response {
    json_response(200, protocols::ollama_models(&catalog(&state).await, true))
}
async fn version() -> Response {
    json_response(200, json!({"version":crate::VERSION,"server":"juiced"}))
}
async fn show(
    State(state): State<Gateway>,
    headers: HeaderMap,
    body: Result<ApiBytes, ApiError>,
) -> Response {
    let result = async {
        let input = parse_body(&headers, body)?;
        let object = input
            .as_object()
            .ok_or_else(|| codec(400, "invalid_request", "Send a model object."))?;
        if object
            .keys()
            .any(|key| key != "model" && key != "name" && key != "verbose")
            || input
                .get("verbose")
                .is_some_and(|value| !value.is_boolean())
            || (input.get("model").is_some() && input.get("name").is_some())
        {
            return Err(codec(
                400,
                "unsupported_parameter",
                "Use model or name and an optional boolean verbose field.",
            ));
        }
        let model = input
            .get("model")
            .or_else(|| input.get("name"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                codec(
                    400,
                    "invalid_request",
                    "A nonempty model string is required.",
                )
            })?;
        let entry = catalog(&state)
            .await
            .into_iter()
            .find(|entry| entry["id"] == model && entry["downloaded"] == true)
            .ok_or_else(|| {
                codec(
                    404,
                    "model_unavailable",
                    "The requested model is not downloaded and enabled.",
                )
            })?;
        Ok::<_, CodecError>(json_response(200, protocols::ollama_show(&entry)?))
    }
    .await;
    result.unwrap_or_else(|error| protocol_error(error, false))
}

fn response_fits(value: &Value) -> bool {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_RESPONSE_BYTES {
                return Err(std::io::Error::other("Response exceeds its size limit"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    serde_json::to_writer(Count(0), value).is_ok()
}

fn cancellation_key(
    context: &RequestContext,
    session: Option<&str>,
    id: &Value,
) -> Result<CancelKey, CodecError> {
    let id = serde_json::to_string(id)
        .map_err(|_| codec(400, "invalid_request", "The request ID cannot be encoded."))?;
    if id.len() > 512 {
        return Err(codec(
            400,
            "invalid_request",
            "MCP request IDs must fit within 512 encoded bytes.",
        ));
    }
    Ok(CancelKey {
        principal: context.principal,
        epoch: context.credential_epoch.clone(),
        session: session.map(str::to_owned),
        id,
    })
}
fn tool_error(id: Value, error: CodecError) -> Value {
    protocols::mcp_result(
        id,
        json!({"content":[{"type":"text","text":error.message}],"structuredContent":{"error":{"code":error.code,"status":error.status}},"isError":true}),
    )
}
async fn mcp_message(
    state: Gateway,
    context: RequestContext,
    headers: HeaderMap,
    message: Value,
) -> Option<Value> {
    match protocols::mcp_request(message) {
        McpDispatch::Reply(value) => Some(value),
        McpDispatch::Notification => None,
        McpDispatch::Cancel { request_id } => {
            if let Ok(session) = request_session(&headers)
                && let Ok(key) = cancellation_key(&context, session, &request_id)
            {
                state.pending.cancel(&key);
            }
            None
        }
        McpDispatch::ListModels { id } => {
            let models: Vec<_> = catalog(&state).await.into_iter().filter(|model| model["policy"] != "disabled" && model["downloaded"] == true).map(|model| {
                let mut entry = json!({"id":model["id"],"object":"model","owned_by":"juiced","operations":model["operations"]});
                if let Some(target) = model.get("aliased_to") {
                    entry["owned_by"] = json!("juiced-alias");
                    entry["aliased_to"] = target.clone();
                }
                entry
            }).collect();
            Some(protocols::mcp_result(
                id,
                json!({"content":[{"type":"text","text":json!({"models":models}).to_string()}],"structuredContent":{"models":models},"isError":false}),
            ))
        }
        McpDispatch::Inference { id, request } => {
            let result = async {
                let session = request_session(&headers).map_err(from_api)?;
                let key = cancellation_key(&context, session, &id)?;
                let (_pending, mut cancellation) = state.pending.register(key)?;
                let work = async {
                    let response = forward(&state, &headers, &request, &context).await?;
                    collect(response).await
                };
                tokio::select! {
                    biased;
                    _ = cancellation.changed() => Err(codec(499, "request_cancelled", "The request was cancelled by the caller.")),
                    value = work => value,
                }
            }.await;
            Some(match result {
                Ok(value) => protocols::mcp_inference_result(id.clone(), &value)
                    .unwrap_or_else(|error| tool_error(id, error)),
                Err(error) if error.code == "request_cancelled" => return None,
                Err(error) => tool_error(id, error),
            })
        }
    }
}
fn mcp_transport(headers: &HeaderMap) -> Result<(), CodecError> {
    if headers.contains_key(header::ORIGIN) {
        return Err(codec(
            403,
            "origin_not_allowed",
            "Browser origins are not enabled for this MCP gateway.",
        ));
    }
    if headers.get_all("mcp-protocol-version").iter().count() > 1
        || headers
            .get("mcp-protocol-version")
            .is_some_and(|value| value != "2025-06-18")
    {
        return Err(codec(
            400,
            "unsupported_protocol",
            "Unsupported MCP protocol version.",
        ));
    }
    if headers.contains_key(header::ACCEPT)
        && !headers
            .get_all(header::ACCEPT)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|value| {
                let mut parts = value.split(';');
                let mime = parts.next().unwrap_or("").trim();
                matches!(mime, "application/json" | "application/*" | "*/*")
                    && !parts.any(|part| {
                        part.trim()
                            .strip_prefix("q=")
                            .and_then(|q| q.parse::<f32>().ok())
                            .is_some_and(|q| q <= 0.0)
                    })
            })
    {
        return Err(codec(
            406,
            "unsupported_response_type",
            "The MCP gateway returns application/json.",
        ));
    }
    Ok(())
}

async fn mcp(
    State(state): State<Gateway>,
    Extension(context): Extension<RequestContext>,
    headers: HeaderMap,
    body: Result<ApiBytes, ApiError>,
) -> Response {
    let body = match body {
        Ok(ApiBytes(body)) => body,
        Err(error) => {
            return json_response(
                error.status.as_u16(),
                protocols::mcp_error(Value::Null, -32600, &error.message),
            );
        }
    };
    if let Err(error) = mcp_transport(&headers) {
        return json_response(
            error.status,
            protocols::mcp_error(Value::Null, -32600, &error.message),
        );
    }
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .eq_ignore_ascii_case("application/json")
        })
    {
        return json_response(
            415,
            protocols::mcp_error(Value::Null, -32600, "Send application/json."),
        );
    }
    if let Err(error) = request_session(&headers) {
        return json_response(
            400,
            protocols::mcp_error(Value::Null, -32600, &error.message),
        );
    }
    let message = match protocols::mcp_decode(&body) {
        Ok(message) => message,
        Err(error) => return json_response(400, error),
    };
    match mcp_message(state, context, headers, message).await {
        Some(response) if response_fits(&response) => json_response(200, response),
        Some(_) => json_response(
            502,
            protocols::mcp_error(
                Value::Null,
                -32603,
                "The MCP response exceeded the size limit.",
            ),
        ),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn method_not_allowed(uri: Uri, headers: HeaderMap) -> Response {
    if uri.path() == "/mcp"
        && let Err(error) = mcp_transport(&headers)
    {
        return json_response(
            error.status,
            protocols::mcp_error(Value::Null, -32600, &error.message),
        );
    }
    let error = codec(
        405,
        "method_not_allowed",
        "This protocol route does not support that HTTP method.",
    );
    let mut response = if uri.path() == "/mcp" {
        json_response(
            405,
            protocols::mcp_error(Value::Null, -32600, &error.message),
        )
    } else {
        protocol_error(error, uri.path().starts_with("/v1/messages"))
    };
    let allow = if matches!(uri.path(), "/api/tags" | "/api/ps" | "/api/version") {
        "GET, HEAD"
    } else {
        "POST"
    };
    response
        .headers_mut()
        .insert(header::ALLOW, allow.parse().unwrap());
    response
}

enum Encoder {
    Anthropic(protocols::AnthropicStream),
    Ollama(protocols::OllamaStream),
}
impl Encoder {
    fn event(&mut self, event: SseEvent) -> Result<Vec<u8>, CodecError> {
        match self {
            Self::Anthropic(encoder) => encoder.event(event),
            Self::Ollama(encoder) => encoder.event(event),
        }
    }
    fn finish(&mut self) -> Result<Vec<u8>, CodecError> {
        match self {
            Self::Anthropic(encoder) => encoder.finish(),
            Self::Ollama(encoder) => encoder.finish(),
        }
    }
    fn error(&self, error: &CodecError) -> Bytes {
        let text = match self {
            Self::Anthropic(_) => format!(
                "event: error\ndata: {}\n\n",
                protocols::anthropic_error(error)
            ),
            Self::Ollama(_) => format!("{}\n", protocols::ollama_error(error)),
        };
        Bytes::from(text)
    }
}
struct Translation {
    source: BodyDataStream,
    decoder: protocols::SseDecoder,
    encoder: Encoder,
    stopped: bool,
}
impl Translation {
    fn events(&mut self, events: Vec<SseEvent>) -> Result<Bytes, CodecError> {
        let mut output = Vec::new();
        for event in events {
            let encoded = self.encoder.event(event)?;
            if output.len() + encoded.len() > MAX_RESPONSE_BYTES {
                return Err(codec(
                    502,
                    "invalid_upstream",
                    "The converted stream chunk exceeded the size limit.",
                ));
            }
            output.extend_from_slice(&encoded);
        }
        Ok(Bytes::from(output))
    }
    fn fail(&mut self, error: CodecError) -> Bytes {
        self.source = Body::empty().into_data_stream();
        self.stopped = true;
        self.encoder.error(&error)
    }
}
fn stream_response(response: Response, encoder: Encoder) -> Response {
    let anthropic = matches!(encoder, Encoder::Anthropic(_));
    let translation = Translation {
        source: response.into_body().into_data_stream(),
        decoder: protocols::SseDecoder::default(),
        encoder,
        stopped: false,
    };
    let body = stream::unfold(translation, |mut state| async move {
        loop {
            if state.stopped {
                return None;
            }
            let result = match state.source.next().await {
                Some(Ok(bytes)) if bytes.len() <= MAX_STREAM_CHUNK => state
                    .decoder
                    .push(&bytes)
                    .and_then(|events| state.events(events)),
                Some(Ok(_)) => Err(codec(
                    502,
                    "invalid_upstream",
                    "The worker stream chunk exceeded the size limit.",
                )),
                Some(Err(_)) => Err(codec(
                    502,
                    "upstream_interrupted",
                    "The worker stream was interrupted.",
                )),
                None => {
                    let result = state
                        .decoder
                        .finish()
                        .and_then(|events| state.events(events))
                        .and_then(|head| {
                            let tail = state.encoder.finish()?;
                            let mut output = Vec::with_capacity(head.len() + tail.len());
                            output.extend_from_slice(&head);
                            output.extend_from_slice(&tail);
                            Ok(Bytes::from(output))
                        });
                    state.stopped = true;
                    result
                }
            };
            match result {
                Ok(bytes) if bytes.is_empty() => continue,
                Ok(bytes) => return Some((Ok::<Bytes, std::io::Error>(bytes), state)),
                Err(error) => {
                    let bytes = state.fail(error);
                    return Some((Ok(bytes), state));
                }
            }
        }
    });
    let mut response = Response::new(Body::from_stream(body));
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        if anthropic {
            "text/event-stream"
        } else {
            "application/x-ndjson"
        }
        .parse()
        .unwrap(),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-accel-buffering", "no".parse().unwrap());
    response
}

fn utc_now() -> String {
    utc_timestamp(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    )
}
fn utc_timestamp(seconds: u64) -> String {
    let days = (seconds / 86400) as i64;
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let day = z - era * 146097;
    let year_of_era = (day - day / 1460 + day / 36524 - day / 146096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let date = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{date:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600 % 24,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_calendar_dates() {
        assert_eq!(utc_timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(1709251199), "2024-02-29T23:59:59Z");
        assert_eq!(utc_timestamp(1709251200), "2024-03-01T00:00:00Z");
    }

    #[tokio::test]
    async fn streaming_conversion_survives_byte_splits_and_reports_truncation() {
        let wire = concat!(
            "data: {\"id\":\"r\",\"choices\":[{\"delta\":{\"content\":\"λ\"},\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let chunks: Vec<_> = wire
            .as_bytes()
            .iter()
            .map(|byte| Ok::<_, std::io::Error>(Bytes::from(vec![*byte])))
            .collect();
        let response = stream_response(
            Response::new(Body::from_stream(stream::iter(chunks))),
            Encoder::Anthropic(protocols::AnthropicStream::new("m")),
        );
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        let body = to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("λ"));
        assert_eq!(text.matches("event: message_stop").count(), 1);
        let response = stream_response(
            Response::new(Body::from(
                "data: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\n",
            )),
            Encoder::Ollama(
                protocols::OllamaStream::new("m", "now", OllamaOperation::Chat).unwrap(),
            ),
        );
        let body = to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
            .await
            .unwrap();
        let text = std::str::from_utf8(&body).unwrap();
        assert!(text.contains("\"error\""));
        assert!(!text.contains("\"done\":true"));
    }

    #[tokio::test]
    async fn stream_error_drops_upstream_immediately() {
        struct DropSignal(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropSignal {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = DropSignal(dropped.clone());
        let source = stream::unfold(Some(guard), |guard| async {
            guard.map(|guard| {
                (
                    Ok::<_, std::io::Error>(Bytes::from_static(b"data: invalid\n\n")),
                    Some(guard),
                )
            })
        });
        let response = stream_response(
            Response::new(Body::from_stream(source)),
            Encoder::Anthropic(protocols::AnthropicStream::new("m")),
        );
        let mut body = response.into_body().into_data_stream();
        let error = body.next().await.unwrap().unwrap();
        assert!(
            std::str::from_utf8(&error)
                .unwrap()
                .contains("event: error")
        );
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn oversized_and_failed_collections_are_protocol_errors() {
        let error = collect(Response::new(Body::from(vec![
            b' ';
            MAX_RESPONSE_BYTES + 1
        ])))
        .await
        .unwrap_err();
        assert_eq!(error.status, 502);
        let mut response = Response::new(Body::from("private worker error"));
        *response.status_mut() = StatusCode::BAD_REQUEST;
        let error = collect(response).await.unwrap_err();
        assert_eq!(error.status, 400);
        assert!(!error.message.contains("private"));
    }

    #[test]
    fn cancellation_keys_separate_credentials_sessions_and_json_id_types() {
        let context = RequestContext {
            principal: "api",
            role: crate::auth::Role::Inference,
            credential_epoch: Arc::from("first"),
        };
        let key = cancellation_key(&context, Some("one"), &json!(1)).unwrap();
        assert_ne!(
            key,
            cancellation_key(&context, Some("one"), &json!("1")).unwrap()
        );
        assert_ne!(
            key,
            cancellation_key(&context, Some("two"), &json!(1)).unwrap()
        );
        let mut other = context.clone();
        other.credential_epoch = Arc::from("second");
        assert_ne!(
            key,
            cancellation_key(&other, Some("one"), &json!(1)).unwrap()
        );
    }
    #[tokio::test]
    async fn cancellation_registry_rejects_replacement_limits_and_releases() {
        let registry = Arc::new(PendingMcp::default());
        let key = CancelKey {
            principal: "api",
            epoch: Arc::from("epoch"),
            session: None,
            id: "request".into(),
        };
        let (guard, mut receiver) = registry.register(key.clone()).unwrap();
        assert!(
            matches!(registry.register(key.clone()), Err(error) if error.code == "duplicate_request")
        );
        let mut other = key.clone();
        other.session = Some("other".into());
        registry.cancel(&other);
        assert!(!receiver.has_changed().unwrap());
        registry.cancel(&key);
        receiver.changed().await.unwrap();
        assert!(*receiver.borrow());
        drop(guard);
        assert!(registry.requests.lock().unwrap().is_empty());
        let mut guards = Vec::new();
        for index in 0..MAX_PENDING_MCP {
            let mut key = key.clone();
            key.id = index.to_string();
            guards.push(registry.register(key).unwrap().0);
        }
        assert!(
            matches!(registry.register(key.clone()), Err(error) if error.code == "request_slots")
        );
        drop(guards);
        assert!(registry.register(key).is_ok());
    }

    #[test]
    fn mcp_transport_requires_safe_origin_and_acceptable_json() {
        let mut headers = HeaderMap::new();
        assert!(mcp_transport(&headers).is_ok());
        headers.insert(header::ACCEPT, "text/event-stream".parse().unwrap());
        assert_eq!(mcp_transport(&headers).unwrap_err().status, 406);
        headers.insert(
            header::ACCEPT,
            "application/json;q=0, text/event-stream".parse().unwrap(),
        );
        assert!(mcp_transport(&headers).is_err());
        headers.insert(
            header::ACCEPT,
            "application/json, text/event-stream".parse().unwrap(),
        );
        assert!(mcp_transport(&headers).is_ok());
        headers.insert(header::ORIGIN, "https://untrusted.example".parse().unwrap());
        assert_eq!(mcp_transport(&headers).unwrap_err().status, 403);
        headers.remove(header::ORIGIN);
        headers.insert("mcp-protocol-version", "2025-03-26".parse().unwrap());
        assert_eq!(mcp_transport(&headers).unwrap_err().status, 400);
    }

    #[test]
    fn response_size_check_counts_escaped_wire_bytes_without_copying() {
        assert!(response_fits(&json!({"value":"λ"})));
        assert!(!response_fits(
            &json!({"value":"\n".repeat(MAX_RESPONSE_BYTES / 2)})
        ));
    }
    #[tokio::test]
    async fn authorization_rejections_use_each_protocol_error_shape() {
        for path in ["/v1/messages", "/api/chat", "/mcp"] {
            let response = rejection(
                path,
                ApiError::new(
                    StatusCode::UNAUTHORIZED,
                    "invalid_key",
                    "Provide valid credentials.",
                ),
            );
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            let body = to_bytes(response.into_body(), MAX_RESPONSE_BYTES)
                .await
                .unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap();
            match path {
                "/v1/messages" => {
                    assert_eq!(body["type"], "error");
                    assert_eq!(body["error"]["type"], "authentication_error");
                }
                "/api/chat" => assert!(body["error"].is_string()),
                _ => assert_eq!(body["jsonrpc"], "2.0"),
            }
        }
    }
}
