use serde_json::{Map, Value, json};
use std::collections::BTreeMap;

const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_TOOL_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl std::fmt::Display for CodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for CodecError {}

fn invalid(message: impl Into<String>) -> CodecError {
    CodecError {
        status: 400,
        code: "invalid_request",
        message: message.into(),
    }
}
fn unsupported(message: impl Into<String>) -> CodecError {
    CodecError {
        status: 400,
        code: "unsupported_parameter",
        message: message.into(),
    }
}
fn upstream(message: impl Into<String>) -> CodecError {
    CodecError {
        status: 502,
        code: "invalid_upstream",
        message: message.into(),
    }
}

fn object(value: &Value) -> Result<&Map<String, Value>, CodecError> {
    value
        .as_object()
        .ok_or_else(|| invalid("Expected a JSON object."))
}
fn fields(value: &Value, allowed: &[&str]) -> Result<(), CodecError> {
    for key in object(value)?.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(unsupported(format!("Unsupported parameter: {key}")));
        }
    }
    Ok(())
}
fn text<'a>(value: &'a Value, name: &str) -> Result<&'a str, CodecError> {
    value
        .get(name)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .ok_or_else(|| invalid(format!("A nonempty {name} string is required.")))
}
fn boolean(value: &Value, name: &str, default: bool) -> Result<bool, CodecError> {
    value.get(name).map_or(Ok(default), |v| {
        v.as_bool()
            .ok_or_else(|| invalid(format!("{name} must be boolean.")))
    })
}
fn array<'a>(value: &'a Value, name: &str) -> Result<&'a Vec<Value>, CodecError> {
    value
        .get(name)
        .and_then(Value::as_array)
        .ok_or_else(|| invalid(format!("{name} must be an array.")))
}
fn copy_number(input: &Value, from: &str, output: &mut Value, to: &str) -> Result<(), CodecError> {
    if let Some(value) = input.get(from) {
        if !value.is_number() {
            return Err(invalid(format!("{from} must be numeric.")));
        }
        output[to] = value.clone();
    }
    Ok(())
}
fn positive_integer(input: &Value, name: &str) -> Result<u64, CodecError> {
    input
        .get(name)
        .and_then(Value::as_u64)
        .filter(|v| *v > 0)
        .ok_or_else(|| invalid(format!("{name} must be a positive integer.")))
}
fn stop_sequences(value: &Value) -> Result<(), CodecError> {
    if value.is_string()
        || value
            .as_array()
            .is_some_and(|values| values.iter().all(Value::is_string))
    {
        Ok(())
    } else {
        Err(invalid("Stop sequences must be text or an array of text."))
    }
}

#[derive(Clone, Debug)]
pub struct ProtocolRequest {
    pub operation: &'static str,
    pub body: Value,
    pub stream: bool,
}

fn text_blocks(value: &Value) -> Result<String, CodecError> {
    if let Some(value) = value.as_str() {
        return Ok(value.to_owned());
    }
    let blocks = value
        .as_array()
        .ok_or_else(|| invalid("Content must be text or an array of text blocks."))?;
    let mut result = String::new();
    for block in blocks {
        fields(block, &["type", "text"])?;
        if block["type"] != "text" {
            return Err(unsupported("Only text blocks are supported in this field."));
        }
        result.push_str(
            block
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("A text block needs a text string."))?,
        );
    }
    Ok(result)
}

pub fn anthropic_request(input: Value, count_tokens: bool) -> Result<ProtocolRequest, CodecError> {
    fields(
        &input,
        &[
            "model",
            "messages",
            "system",
            "max_tokens",
            "temperature",
            "top_p",
            "top_k",
            "stop_sequences",
            "stream",
            "tools",
            "tool_choice",
            "thinking",
            "output_config",
        ],
    )?;
    text(&input, "model")?;
    let stream = boolean(&input, "stream", false)?;
    if count_tokens && stream {
        return Err(invalid("Token counting cannot stream."));
    }
    if !count_tokens {
        positive_integer(&input, "max_tokens")?;
    }
    let mut messages = Vec::new();
    if let Some(system) = input.get("system") {
        messages.push(json!({"role":"system", "content":text_blocks(system)?}));
    }
    let source_messages = array(&input, "messages")?;
    if source_messages.is_empty() {
        return Err(invalid("messages must not be empty."));
    }
    for message in source_messages {
        fields(message, &["role", "content"])?;
        let role = text(message, "role")?;
        if !["user", "assistant"].contains(&role) {
            return Err(invalid("Messages roles must be user or assistant."));
        }
        if message["content"].is_string() {
            messages.push(json!({"role":role,"content":message["content"]}));
            continue;
        }
        let blocks = array(message, "content")?;
        let mut parts = Vec::new();
        let mut tool_calls = Vec::new();
        let mut results = Vec::new();
        for block in blocks {
            match text(block, "type")? {
                "text" => {
                    fields(block, &["type", "text"])?;
                    let value = block
                        .get("text")
                        .and_then(Value::as_str)
                        .ok_or_else(|| invalid("Text content must be a string."))?;
                    parts.push(json!({"type":"text","text":value}));
                }
                "image" => {
                    fields(block, &["type", "source"])?;
                    let source = &block["source"];
                    let url = match text(source, "type")? {
                        "base64" => {
                            fields(source, &["type", "media_type", "data"])?;
                            let media = text(source, "media_type")?;
                            if !["image/png", "image/jpeg", "image/webp", "image/gif"]
                                .contains(&media)
                            {
                                return Err(unsupported("Unsupported image media type."));
                            }
                            format!("data:{media};base64,{}", text(source, "data")?)
                        }
                        "url" => {
                            fields(source, &["type", "url"])?;
                            let url = text(source, "url")?;
                            if !url.starts_with("https://") && !url.starts_with("http://") {
                                return Err(invalid("Image URLs must use HTTP or HTTPS."));
                            }
                            url.to_owned()
                        }
                        _ => return Err(unsupported("Unsupported image source.")),
                    };
                    parts.push(json!({"type":"image_url","image_url":{"url":url}}));
                }
                "tool_use" => {
                    fields(block, &["type", "id", "name", "input"])?;
                    if role != "assistant" || !block["input"].is_object() {
                        return Err(invalid(
                            "tool_use needs assistant role and an object input.",
                        ));
                    }
                    tool_calls.push(json!({"id":text(block,"id")?,"type":"function","function":{"name":text(block,"name")?,"arguments":serde_json::to_string(&block["input"]).unwrap()}}));
                }
                "tool_result" => {
                    fields(block, &["type", "tool_use_id", "content", "is_error"])?;
                    if role != "user" {
                        return Err(invalid("tool_result needs user role."));
                    }
                    if boolean(block, "is_error", false)? {
                        return Err(unsupported(
                            "Tool error flags cannot be translated through OpenAI tool messages.",
                        ));
                    }
                    if !parts.is_empty() {
                        return Err(invalid("Tool results must precede user text or images."));
                    }
                    let content = match block.get("content") {
                        Some(value) => text_blocks(value)?,
                        None => String::new(),
                    };
                    results.push(json!({"role":"tool","tool_call_id":text(block,"tool_use_id")?,"content":content}));
                }
                "thinking" | "redacted_thinking" => {
                    return Err(unsupported(
                        "Signed prior thinking blocks cannot be translated through OpenAI.",
                    ));
                }
                _ => return Err(unsupported("Unsupported Messages content block.")),
            }
        }
        let has_results = !results.is_empty();
        messages.extend(results);
        if !parts.is_empty() || !tool_calls.is_empty() || !has_results {
            let content = if parts.iter().all(|part| part["type"] == "text") {
                Value::String(
                    parts
                        .iter()
                        .filter_map(|part| part["text"].as_str())
                        .collect(),
                )
            } else {
                Value::Array(parts)
            };
            let mut converted = json!({"role":role,"content":content});
            if !tool_calls.is_empty() {
                converted["tool_calls"] = Value::Array(tool_calls);
            }
            messages.push(converted);
        }
    }
    let mut output = json!({"model":input["model"], "messages":messages, "stream":stream});
    if let Some(max) = input.get("max_tokens") {
        if max.as_u64().is_none_or(|n| n == 0) {
            return Err(invalid("max_tokens must be positive."));
        }
        output["max_tokens"] = max.clone();
    }
    for key in ["temperature", "top_p", "top_k"] {
        copy_number(&input, key, &mut output, key)?;
    }
    if let Some(stops) = input.get("stop_sequences") {
        stop_sequences(stops)?;
        output["stop"] = stops.clone();
    }
    if let Some(tools) = input.get("tools") {
        let mut converted = Vec::new();
        for tool in tools
            .as_array()
            .ok_or_else(|| invalid("tools must be an array."))?
        {
            fields(tool, &["name", "description", "input_schema"])?;
            if !tool["input_schema"].is_object() {
                return Err(invalid("Tool input_schema must be an object."));
            }
            if tool
                .get("description")
                .is_some_and(|value| !value.is_string())
            {
                return Err(invalid("Tool description must be text."));
            }
            converted.push(json!({"type":"function","function":{"name":text(tool,"name")?,"description":tool.get("description").cloned().unwrap_or(json!("")),"parameters":tool["input_schema"]}}));
        }
        output["tools"] = Value::Array(converted);
    }
    if let Some(choice) = input.get("tool_choice") {
        fields(choice, &["type", "name", "disable_parallel_tool_use"])?;
        if let Some(disabled) = choice.get("disable_parallel_tool_use") {
            output["parallel_tool_calls"] = json!(
                !disabled
                    .as_bool()
                    .ok_or_else(|| invalid("disable_parallel_tool_use must be boolean."))?
            );
        }
        if choice.get("name").is_some() && choice["type"] != "tool" {
            return Err(invalid("A tool-choice name requires type tool."));
        }
        output["tool_choice"] = match text(choice, "type")? {
            "auto" => json!("auto"),
            "any" => json!("required"),
            "none" => json!("none"),
            "tool" => json!({"type":"function","function":{"name":text(choice,"name")?}}),
            _ => return Err(unsupported("Unsupported tool choice.")),
        };
    }
    if let Some(thinking) = input.get("thinking") {
        fields(thinking, &["type"])?;
        output["enable_thinking"] = match text(thinking, "type")? {
            "enabled" => json!(true),
            "disabled" => json!(false),
            _ => return Err(unsupported("Unsupported thinking mode.")),
        };
    }
    if let Some(config) = input.get("output_config") {
        fields(config, &["format"])?;
        let format = &config["format"];
        fields(format, &["type", "schema"])?;
        output["response_format"] = match text(format, "type")? {
            "json_object" if format.get("schema").is_none() => json!({"type":"json_object"}),
            "json_schema" if format["schema"].is_object() => {
                json!({"type":"json_schema","json_schema":{"name":"response","schema":format["schema"]}})
            }
            _ => return Err(unsupported("Unsupported output format.")),
        };
    }
    if stream {
        output["stream_options"] = json!({"include_usage":true});
    }
    if count_tokens {
        Ok(ProtocolRequest {
            operation: "messages/count_tokens",
            body: input,
            stream: false,
        })
    } else {
        Ok(ProtocolRequest {
            operation: "chat/completions",
            body: output,
            stream,
        })
    }
}

fn first_choice(value: &Value) -> Result<&Value, CodecError> {
    value
        .get("choices")
        .and_then(Value::as_array)
        .filter(|choices| choices.len() == 1)
        .and_then(|choices| choices.first())
        .ok_or_else(|| upstream("The worker must return one completion choice."))
}
fn token_usage(value: &Value) -> Result<(Option<u64>, Option<u64>), CodecError> {
    let usage = value.get("usage");
    let get = |name| -> Result<Option<u64>, CodecError> {
        usage
            .and_then(|usage| usage.get(name))
            .map(|count| {
                count
                    .as_u64()
                    .ok_or_else(|| upstream("Invalid worker token usage."))
            })
            .transpose()
    };
    Ok((get("prompt_tokens")?, get("completion_tokens")?))
}
fn stop_reason(reason: Option<&str>) -> Result<&'static str, CodecError> {
    match reason {
        Some("stop") | None => Ok("end_turn"),
        Some("length") => Ok("max_tokens"),
        Some("tool_calls") | Some("function_call") => Ok("tool_use"),
        Some("content_filter") => Ok("refusal"),
        _ => Err(upstream("Unsupported worker finish reason.")),
    }
}
fn parsed_arguments(call: &Value) -> Result<Value, CodecError> {
    match call
        .get("function")
        .and_then(|function| function.get("arguments"))
    {
        Some(Value::String(value)) => {
            let value: Value = serde_json::from_str(value)
                .map_err(|_| upstream("Worker tool arguments are invalid JSON."))?;
            if !value.is_object() {
                return Err(upstream("Worker tool arguments must be an object."));
            }
            Ok(value)
        }
        Some(value) if value.is_object() => Ok(value.clone()),
        _ => Err(upstream("Worker tool arguments are unavailable.")),
    }
}

pub fn anthropic_response(input: &Value) -> Result<Value, CodecError> {
    let choice = first_choice(input)?;
    let message = choice
        .get("message")
        .and_then(Value::as_object)
        .ok_or_else(|| upstream("Worker message is unavailable."))?;
    let mut blocks = Vec::new();
    if let Some(reasoning) = message.get("reasoning_content") {
        let reasoning = reasoning
            .as_str()
            .ok_or_else(|| upstream("Worker reasoning must be text."))?;
        if !reasoning.is_empty() {
            blocks.push(json!({"type":"thinking","thinking":reasoning}));
        }
    }
    if let Some(content) = message.get("content").filter(|value| !value.is_null()) {
        if let Some(content) = content.as_str() {
            if !content.is_empty() {
                blocks.push(json!({"type":"text","text":content}));
            }
        } else {
            return Err(upstream("Unsupported worker response content."));
        }
    }
    if let Some(calls) = message.get("tool_calls") {
        for call in calls
            .as_array()
            .ok_or_else(|| upstream("Worker tool_calls must be an array."))?
        {
            let id =
                text(call, "id").map_err(|_| upstream("Worker tool call ID is unavailable."))?;
            let name = text(&call["function"], "name")
                .map_err(|_| upstream("Worker tool name is unavailable."))?;
            blocks.push(
                json!({"type":"tool_use","id":id,"name":name,"input":parsed_arguments(call)?}),
            );
        }
    }
    if blocks.is_empty() {
        blocks.push(json!({"type":"text","text":""}));
    }
    let (input_tokens, output_tokens) = token_usage(input)?;
    let reason = if blocks.iter().any(|block| block["type"] == "tool_use") {
        "tool_use"
    } else {
        stop_reason(choice["finish_reason"].as_str())?
    };
    Ok(
        json!({"id":input.get("id").cloned().unwrap_or(Value::Null),"type":"message","role":"assistant","model":input.get("model").cloned().unwrap_or(Value::Null),"content":blocks,"stop_reason":reason,"stop_sequence":null,"usage":{"input_tokens":input_tokens,"output_tokens":output_tokens}}),
    )
}

pub fn anthropic_token_count(input: &Value) -> Result<Value, CodecError> {
    let count = input
        .get("input_tokens")
        .and_then(Value::as_u64)
        .ok_or_else(|| upstream("The worker did not return native Messages token counts."))?;
    Ok(json!({"input_tokens":count}))
}

pub fn anthropic_error(error: &CodecError) -> Value {
    let kind = match error.status {
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        529 => "overloaded_error",
        500.. => "api_error",
        _ => "invalid_request_error",
    };
    json!({"type":"error","error":{"type":kind,"message":error.message}})
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OllamaOperation {
    Chat,
    Generate,
    Embed,
    LegacyEmbed,
}

pub fn ollama_request(
    input: Value,
    operation: OllamaOperation,
) -> Result<ProtocolRequest, CodecError> {
    let allowed: &[&str] = match operation {
        OllamaOperation::Chat => &[
            "model", "messages", "stream", "options", "format", "think", "tools",
        ],
        OllamaOperation::Generate => &["model", "prompt", "stream", "options", "format", "raw"],
        OllamaOperation::Embed => &["model", "input", "options", "truncate"],
        OllamaOperation::LegacyEmbed => &["model", "prompt", "input", "options"],
    };
    fields(&input, allowed)?;
    text(&input, "model")?;
    let stream = match operation {
        OllamaOperation::Chat | OllamaOperation::Generate => boolean(&input, "stream", true)?,
        _ => false,
    };
    let mut output = json!({"model":input["model"],"stream":stream});
    let route = match operation {
        OllamaOperation::Chat => {
            let mut messages = Vec::new();
            for message in array(&input, "messages")? {
                fields(
                    message,
                    &["role", "content", "images", "tool_calls", "tool_call_id"],
                )?;
                let role = text(message, "role")?;
                if !["system", "user", "assistant", "tool"].contains(&role) {
                    return Err(invalid("Unsupported chat message role."));
                }
                let content = message
                    .get("content")
                    .and_then(Value::as_str)
                    .ok_or_else(|| invalid("Message content must be text."))?;
                let mut converted = json!({"role":role,"content":content});
                if let Some(images) = message.get("images") {
                    let images = images
                        .as_array()
                        .ok_or_else(|| invalid("images must be an array of base64 strings."))?;
                    let mut parts = vec![json!({"type":"text","text":content})];
                    for image in images {
                        parts.push(json!({"type":"image_url","image_url":{"url":format!("data:image/png;base64,{}", image.as_str().filter(|s| !s.is_empty()).ok_or_else(|| invalid("Images must be nonempty base64 strings."))?)}}));
                    }
                    converted["content"] = Value::Array(parts);
                }
                if let Some(calls) = message.get("tool_calls") {
                    if role != "assistant" {
                        return Err(invalid("tool_calls requires assistant role."));
                    }
                    let calls = calls
                        .as_array()
                        .ok_or_else(|| invalid("tool_calls must be an array."))?;
                    let mut normalized = Vec::new();
                    for (index, call) in calls.iter().enumerate() {
                        fields(call, &["id", "type", "function"])?;
                        if call.get("type").is_some_and(|kind| kind != "function")
                            || call
                                .get("id")
                                .is_some_and(|id| id.as_str().is_none_or(str::is_empty))
                        {
                            return Err(invalid(
                                "Tool calls need function type and nonempty text IDs when supplied.",
                            ));
                        }
                        fields(&call["function"], &["name", "arguments"])?;
                        if !call["function"]["arguments"].is_object() {
                            return Err(invalid("Ollama tool arguments must be an object."));
                        }
                        normalized.push(json!({"id":call.get("id").cloned().unwrap_or(json!(format!("call_{index}"))),"type":"function","function":{"name":text(&call["function"],"name")?,"arguments":serde_json::to_string(&call["function"]["arguments"]).unwrap()}}));
                    }
                    converted["tool_calls"] = Value::Array(normalized);
                }
                if role == "tool" {
                    converted["tool_call_id"] = json!(text(message, "tool_call_id")?);
                } else if message.get("tool_call_id").is_some() {
                    return Err(invalid("tool_call_id requires tool role."));
                }
                messages.push(converted);
            }
            if messages.is_empty() {
                return Err(invalid("messages must not be empty."));
            }
            output["messages"] = Value::Array(messages);
            if let Some(tools) = input.get("tools") {
                validate_openai_tools(tools)?;
                output["tools"] = tools.clone();
            }
            if let Some(thinking) = input.get("think") {
                if !thinking.is_boolean() {
                    return Err(unsupported("think must be boolean."));
                }
                output["enable_thinking"] = thinking.clone();
            }
            "chat/completions"
        }
        OllamaOperation::Generate => {
            let prompt = input
                .get("prompt")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("prompt must be text."))?;
            if input.get("raw").is_some() && !boolean(&input, "raw", false)? {
                return Err(unsupported(
                    "raw=false requires a chat-template-aware generate contract.",
                ));
            }
            output["prompt"] = json!(prompt);
            "completions"
        }
        OllamaOperation::Embed | OllamaOperation::LegacyEmbed => {
            if input.get("prompt").is_some() && input.get("input").is_some() {
                return Err(invalid("Provide prompt or input, not both."));
            }
            let value = if operation == OllamaOperation::LegacyEmbed {
                input.get("prompt").or_else(|| input.get("input"))
            } else {
                input.get("input")
            }
            .ok_or_else(|| invalid("Embedding input is required."))?;
            if !value.is_string()
                && !value
                    .as_array()
                    .is_some_and(|items| !items.is_empty() && items.iter().all(Value::is_string))
            {
                return Err(invalid(
                    "Embedding input must be text or a nonempty text array.",
                ));
            }
            if operation == OllamaOperation::LegacyEmbed && !value.is_string() {
                return Err(invalid("Legacy embeddings accepts one prompt."));
            }
            if input.get("truncate").is_some() {
                return Err(unsupported(
                    "Embedding truncation requires a provider-qualified contract.",
                ));
            }
            output.as_object_mut().unwrap().remove("stream");
            output["input"] = value.clone();
            "embeddings"
        }
    };
    if let Some(options) = input.get("options") {
        if matches!(
            operation,
            OllamaOperation::Embed | OllamaOperation::LegacyEmbed
        ) && !object(options)?.is_empty()
        {
            return Err(unsupported(
                "Embedding options require a provider-qualified contract.",
            ));
        }
        fields(
            options,
            &[
                "temperature",
                "top_p",
                "top_k",
                "seed",
                "stop",
                "num_predict",
                "presence_penalty",
                "frequency_penalty",
            ],
        )?;
        for (from, to) in [
            ("temperature", "temperature"),
            ("top_p", "top_p"),
            ("top_k", "top_k"),
            ("seed", "seed"),
            ("presence_penalty", "presence_penalty"),
            ("frequency_penalty", "frequency_penalty"),
        ] {
            copy_number(options, from, &mut output, to)?;
        }
        if options.get("num_predict").is_some() {
            output["max_tokens"] = json!(positive_integer(options, "num_predict")?);
        }
        if let Some(stop) = options.get("stop") {
            stop_sequences(stop)?;
            output["stop"] = stop.clone();
        }
    }
    if let Some(format) = input.get("format") {
        output["response_format"] = if format == "json" {
            json!({"type":"json_object"})
        } else if format.is_object() {
            json!({"type":"json_schema","json_schema":{"name":"response","schema":format}})
        } else {
            return Err(unsupported("format must be json or a JSON schema object."));
        };
    }
    if stream {
        output["stream_options"] = json!({"include_usage":true});
    }
    Ok(ProtocolRequest {
        operation: route,
        body: output,
        stream,
    })
}

fn validate_openai_tools(tools: &Value) -> Result<(), CodecError> {
    for tool in tools
        .as_array()
        .ok_or_else(|| invalid("tools must be an array."))?
    {
        fields(tool, &["type", "function"])?;
        if tool["type"] != "function" {
            return Err(unsupported(
                "Only client-executed function tools are supported.",
            ));
        }
        fields(
            &tool["function"],
            &["name", "description", "parameters", "strict"],
        )?;
        text(&tool["function"], "name")?;
        if !tool["function"]["parameters"].is_object() {
            return Err(invalid("Tool parameters must be an object schema."));
        }
        if tool["function"]
            .get("description")
            .is_some_and(|value| !value.is_string())
            || tool["function"]
                .get("strict")
                .is_some_and(|value| !value.is_boolean())
        {
            return Err(invalid(
                "Tool description and strict fields have invalid types.",
            ));
        }
    }
    Ok(())
}

pub fn ollama_response(
    input: &Value,
    operation: OllamaOperation,
    model: &str,
    created_at: &str,
) -> Result<Value, CodecError> {
    if [OllamaOperation::Embed, OllamaOperation::LegacyEmbed].contains(&operation) {
        let mut vectors = Vec::new();
        for item in array(input, "data").map_err(|_| upstream("Embedding data is unavailable."))? {
            let vector = item
                .get("embedding")
                .and_then(Value::as_array)
                .filter(|vector| vector.iter().all(Value::is_number))
                .ok_or_else(|| upstream("Invalid embedding vector."))?;
            vectors.push(Value::Array(vector.clone()));
        }
        if operation == OllamaOperation::LegacyEmbed {
            if vectors.len() != 1 {
                return Err(upstream("Legacy embeddings requires one vector."));
            }
            return Ok(json!({"embedding":vectors.remove(0)}));
        }
        let mut result = json!({"model":model,"embeddings":vectors});
        if let Some(count) = token_usage(input)?.0 {
            result["prompt_eval_count"] = json!(count);
        }
        return Ok(result);
    }
    let choice = first_choice(input)?;
    let mut result = json!({"model":model,"created_at":created_at,"done":true,"done_reason":choice.get("finish_reason").cloned().unwrap_or(Value::Null)});
    if operation == OllamaOperation::Chat {
        result["message"] = ollama_message(&choice["message"])?;
    } else {
        result["response"] = json!(
            choice
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| upstream("Completion text is unavailable."))?
        );
    }
    let (prompt, completion) = token_usage(input)?;
    if let Some(count) = prompt {
        result["prompt_eval_count"] = json!(count);
    }
    if let Some(count) = completion {
        result["eval_count"] = json!(count);
    }
    Ok(result)
}

fn ollama_message(message: &Value) -> Result<Value, CodecError> {
    object(message).map_err(|_| upstream("Worker message is unavailable."))?;
    let content = match message.get("content") {
        Some(Value::String(text)) => text.as_str(),
        Some(Value::Null) | None => "",
        _ => return Err(upstream("Worker content must be text.")),
    };
    let mut result = json!({"role":"assistant","content":content});
    if let Some(thinking) = message.get("reasoning_content") {
        if !thinking.is_string() {
            return Err(upstream("Worker reasoning must be text."));
        }
        result["thinking"] = thinking.clone();
    }
    if let Some(calls) = message.get("tool_calls") {
        let mut output = Vec::new();
        for call in calls
            .as_array()
            .ok_or_else(|| upstream("Worker tool_calls must be an array."))?
        {
            output.push(json!({"function":{"name":text(&call["function"],"name").map_err(|_| upstream("Worker tool name is unavailable."))?,"arguments":parsed_arguments(call)?}}));
        }
        result["tool_calls"] = Value::Array(output);
    }
    Ok(result)
}

pub fn ollama_models(models: &[Value], loaded_only: bool) -> Value {
    let entries: Vec<_> = models
        .iter()
        .filter(|model| {
            model["policy"] != "disabled"
                && model["downloaded"] == true
                && (!loaded_only || model["loaded"] == true)
        })
        .map(|model| {
            let mut item = json!({"name":model["id"],"model":model["id"]});
            for key in ["digest", "size", "modified_at", "details"] {
                if let Some(value) = model.get(key) {
                    item[key] = value.clone();
                }
            }
            item
        })
        .collect();
    json!({"models":entries})
}

pub fn ollama_show(model: &Value) -> Result<Value, CodecError> {
    text(model, "id")?;
    if model["policy"] == "disabled" {
        return Err(CodecError {
            status: 404,
            code: "model_unavailable",
            message: "The requested model is not enabled.".into(),
        });
    }
    let capabilities: Vec<_> = model
        .get("operations")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|op| match op {
            "chat/completions" | "completions" => Some("completion"),
            "embeddings" => Some("embedding"),
            _ => None,
        })
        .collect();
    let mut capabilities = capabilities;
    capabilities.sort_unstable();
    capabilities.dedup();
    Ok(
        json!({"model_info":{"juiced.model":model["id"],"juiced.qualification":"unknown"},"capabilities":capabilities}),
    )
}

pub fn ollama_error(error: &CodecError) -> Value {
    json!({"error":error.message,"code":error.code})
}

#[derive(Debug)]
pub enum McpDispatch {
    Reply(Value),
    Notification,
    Cancel { request_id: Value },
    ListModels { id: Value },
    Inference { id: Value, request: ProtocolRequest },
}

pub fn mcp_error(id: Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
pub fn mcp_result(id: Value, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
pub fn mcp_decode(bytes: &[u8]) -> Result<Value, Value> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Array(_)) => Err(mcp_error(
            Value::Null,
            -32600,
            "MCP 2025-06-18 does not support JSON-RPC batches.",
        )),
        Ok(value @ Value::Object(_)) => Ok(value),
        Ok(_) => Err(mcp_error(
            Value::Null,
            -32600,
            "Send a single JSON-RPC object.",
        )),
        Err(_) => Err(mcp_error(Value::Null, -32700, "Invalid JSON.")),
    }
}

pub fn mcp_request(mut input: Value) -> McpDispatch {
    let id = input.get("id").cloned().unwrap_or(Value::Null);
    if !input.is_object()
        || input["jsonrpc"] != "2.0"
        || input
            .get("id")
            .is_some_and(|id| !id.is_null() && !id.is_string() && !id.is_i64() && !id.is_u64())
    {
        return McpDispatch::Reply(mcp_error(
            Value::Null,
            -32600,
            "Expected a JSON-RPC 2.0 request.",
        ));
    }
    let Some(method) = input.get("method").and_then(Value::as_str) else {
        return McpDispatch::Reply(mcp_error(id, -32600, "A method string is required."));
    };
    let method = method.to_owned();
    if fields(&input, &["jsonrpc", "id", "method", "params"]).is_err() {
        return McpDispatch::Reply(mcp_error(id, -32600, "Unexpected JSON-RPC request fields."));
    }
    let params = input
        .as_object_mut()
        .unwrap()
        .remove("params")
        .unwrap_or(json!({}));
    if !input.as_object().unwrap().contains_key("id") {
        return if method == "notifications/cancelled" {
            match params
                .get("requestId")
                .filter(|id| id.is_string() || id.is_i64() || id.is_u64())
            {
                Some(id) => McpDispatch::Cancel {
                    request_id: id.clone(),
                },
                None => McpDispatch::Notification,
            }
        } else {
            McpDispatch::Notification
        };
    }
    if !params.is_object() {
        return McpDispatch::Reply(mcp_error(id, -32602, "params must be an object."));
    }
    match method.as_str() {
        "initialize" => McpDispatch::Reply(mcp_result(
            id,
            json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"juiced-mcp","version":env!("CARGO_PKG_VERSION")}}),
        )),
        "ping" => McpDispatch::Reply(mcp_result(id, json!({}))),
        "tools/list" => McpDispatch::Reply(mcp_result(id, mcp_tools())),
        "tools/call" => {
            if fields(&params, &["name", "arguments", "_meta"]).is_err() {
                return McpDispatch::Reply(mcp_error(
                    id,
                    -32602,
                    "Unsupported tools/call parameters.",
                ));
            }
            let name = match text(&params, "name") {
                Ok(name) => name,
                Err(_) => {
                    return McpDispatch::Reply(mcp_error(
                        id,
                        -32602,
                        "tools/call requires a name.",
                    ));
                }
            };
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !args.is_object() {
                return McpDispatch::Reply(mcp_error(
                    id,
                    -32602,
                    "Tool arguments must be an object.",
                ));
            }
            if name == "lemonade_list_models" {
                return if object(&args).unwrap().is_empty() {
                    McpDispatch::ListModels { id }
                } else {
                    McpDispatch::Reply(mcp_error(id, -32602, "Model listing accepts no arguments."))
                };
            }
            let (operation, allowed, required): (&str, &[&str], &[&str]) = match name {
                "lemonade_chat" => (
                    "chat/completions",
                    &[
                        "model",
                        "messages",
                        "temperature",
                        "top_p",
                        "max_tokens",
                        "stop",
                        "seed",
                        "tools",
                        "tool_choice",
                        "response_format",
                    ],
                    &["model", "messages"],
                ),
                "lemonade_complete" => (
                    "completions",
                    &[
                        "model",
                        "prompt",
                        "temperature",
                        "top_p",
                        "max_tokens",
                        "stop",
                        "seed",
                    ],
                    &["model", "prompt"],
                ),
                "lemonade_embed" => ("embeddings", &["model", "input"], &["model", "input"]),
                _ => {
                    return McpDispatch::Reply(mcp_result(
                        id,
                        json!({"content":[{"type":"text","text":"Unknown or unsupported inference tool."}],"isError":true}),
                    ));
                }
            };
            if fields(&args, allowed).is_err()
                || required
                    .iter()
                    .any(|key| !args.as_object().unwrap().contains_key(*key))
                || text(&args, "model").is_err()
            {
                return McpDispatch::Reply(mcp_error(
                    id,
                    -32602,
                    "Tool arguments do not match the inference schema.",
                ));
            }
            let valid = match operation {
                "chat/completions" => args["messages"].as_array().is_some_and(|messages| {
                    !messages.is_empty()
                        && messages.iter().all(|message| {
                            message.is_object()
                                && message["role"].is_string()
                                && (message["content"].is_string()
                                    || message["content"].is_array()
                                    || message["content"].is_null())
                        })
                }),
                "completions" => args["prompt"].is_string(),
                _ => {
                    args["input"].is_string()
                        || args["input"]
                            .as_array()
                            .is_some_and(|values| values.iter().all(Value::is_string))
                }
            };
            if !valid {
                return McpDispatch::Reply(mcp_error(id, -32602, "Invalid inference input."));
            }
            let parameters_valid = ["temperature", "top_p"]
                .iter()
                .all(|key| args.get(*key).is_none_or(Value::is_number))
                && args
                    .get("max_tokens")
                    .is_none_or(|value| value.as_u64().is_some_and(|count| count > 0))
                && args
                    .get("seed")
                    .is_none_or(|value| value.is_i64() || value.is_u64())
                && args
                    .get("stop")
                    .is_none_or(|value| stop_sequences(value).is_ok())
                && args
                    .get("tools")
                    .is_none_or(|value| validate_openai_tools(value).is_ok())
                && args.get("response_format").is_none_or(Value::is_object);
            if !parameters_valid {
                return McpDispatch::Reply(mcp_error(id, -32602, "Invalid inference parameters."));
            }
            let mut body = args;
            body["stream"] = json!(false);
            McpDispatch::Inference {
                id,
                request: ProtocolRequest {
                    operation: match operation {
                        "chat/completions" => "chat/completions",
                        "completions" => "completions",
                        _ => "embeddings",
                    },
                    body,
                    stream: false,
                },
            }
        }
        _ => McpDispatch::Reply(mcp_error(id, -32601, "Method not found.")),
    }
}

pub fn mcp_tools() -> Value {
    json!({"tools":[
        {"name":"lemonade_list_models","description":"List policy-enabled downloaded model services.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"lemonade_chat","description":"Run one chat request. Tools execute in the client, never here.","inputSchema":{"type":"object","required":["model","messages"],"properties":{"model":{"type":"string","minLength":1},"messages":{"type":"array","minItems":1},"temperature":{"type":"number"},"top_p":{"type":"number"},"max_tokens":{"type":"integer","minimum":1},"stop":{},"seed":{"type":"integer"},"tools":{"type":"array"},"tool_choice":{},"response_format":{"type":"object"}},"additionalProperties":false}},
        {"name":"lemonade_complete","description":"Run one prompt completion.","inputSchema":{"type":"object","required":["model","prompt"],"properties":{"model":{"type":"string","minLength":1},"prompt":{"type":"string"},"temperature":{"type":"number"},"top_p":{"type":"number"},"max_tokens":{"type":"integer","minimum":1},"stop":{},"seed":{"type":"integer"}},"additionalProperties":false}},
        {"name":"lemonade_embed","description":"Embed text using one enabled embedding model.","inputSchema":{"type":"object","required":["model","input"],"properties":{"model":{"type":"string","minLength":1},"input":{}},"additionalProperties":false}}
    ]})
}

pub fn mcp_inference_result(id: Value, response: &Value) -> Result<Value, CodecError> {
    if response.get("error").is_some() {
        return Ok(mcp_result(
            id,
            json!({"content":[{"type":"text","text":"The model service request failed."}],"isError":true}),
        ));
    }
    Ok(mcp_result(
        id,
        json!({"content":[{"type":"text","text":serde_json::to_string(response).map_err(|_| upstream("Cannot encode the model result."))?}],"structuredContent":response,"isError":false}),
    ))
}

#[derive(Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

#[derive(Default)]
pub struct SseDecoder {
    line: Vec<u8>,
    data: Vec<u8>,
    event: Option<String>,
    frame_bytes: usize,
}

impl SseDecoder {
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<SseEvent>, CodecError> {
        let mut output = Vec::new();
        for &byte in bytes {
            if byte == b'\n' {
                self.line(&mut output)?;
            } else {
                self.line.push(byte);
                if self.line.len() > MAX_FRAME_BYTES {
                    return Err(upstream("SSE line exceeds the frame limit."));
                }
            }
        }
        Ok(output)
    }
    fn line(&mut self, output: &mut Vec<SseEvent>) -> Result<(), CodecError> {
        if self.line.last() == Some(&b'\r') {
            self.line.pop();
        }
        if self.line.is_empty() {
            if !self.data.is_empty() {
                if self.data.last() == Some(&b'\n') {
                    self.data.pop();
                }
                output.push(SseEvent {
                    event: self.event.take(),
                    data: String::from_utf8(std::mem::take(&mut self.data))
                        .map_err(|_| upstream("SSE data is not UTF-8."))?,
                });
            }
            self.frame_bytes = 0;
            self.event = None;
            return Ok(());
        }
        self.frame_bytes = self
            .frame_bytes
            .checked_add(self.line.len())
            .ok_or_else(|| upstream("SSE frame overflow."))?;
        if self.frame_bytes > MAX_FRAME_BYTES {
            return Err(upstream("SSE frame exceeds the frame limit."));
        }
        if !self.line.starts_with(b":") {
            let split = self
                .line
                .iter()
                .position(|&byte| byte == b':')
                .unwrap_or(self.line.len());
            let key = &self.line[..split];
            let mut value = if split == self.line.len() {
                &[][..]
            } else {
                &self.line[split + 1..]
            };
            if value.first() == Some(&b' ') {
                value = &value[1..];
            }
            if key == b"data" {
                self.data.extend_from_slice(value);
                self.data.push(b'\n');
            }
            if key == b"event" {
                self.event = Some(
                    std::str::from_utf8(value)
                        .map_err(|_| upstream("SSE event is not UTF-8."))?
                        .to_owned(),
                );
            }
        }
        self.line.clear();
        Ok(())
    }
    pub fn finish(&mut self) -> Result<Vec<SseEvent>, CodecError> {
        let mut events = Vec::new();
        if !self.line.is_empty() {
            self.line(&mut events)?;
        }
        if !self.data.is_empty() {
            self.line(&mut events)?;
        }
        Ok(events)
    }
}

#[derive(Default)]
struct ToolFragment {
    id: String,
    name: String,
    arguments: String,
}
fn accumulate_tools(
    value: &Value,
    tools: &mut BTreeMap<u64, ToolFragment>,
) -> Result<(), CodecError> {
    let Some(calls) = value.get("tool_calls") else {
        return Ok(());
    };
    for call in calls
        .as_array()
        .ok_or_else(|| upstream("Streaming tool_calls must be an array."))?
    {
        let index = call["index"]
            .as_u64()
            .filter(|index| *index < 128)
            .ok_or_else(|| upstream("Invalid streaming tool index."))?;
        let tool = tools.entry(index).or_default();
        for (field, target) in [
            (call.get("id"), &mut tool.id),
            (
                call.get("function").and_then(|v| v.get("name")),
                &mut tool.name,
            ),
            (
                call.get("function").and_then(|v| v.get("arguments")),
                &mut tool.arguments,
            ),
        ] {
            if let Some(field) = field {
                target.push_str(
                    field
                        .as_str()
                        .ok_or_else(|| upstream("Streaming tool fragments must be text."))?,
                );
                if target.len() > MAX_TOOL_BYTES {
                    return Err(upstream("Streaming tool fragment exceeds its limit."));
                }
            }
        }
    }
    let total: usize = tools
        .values()
        .map(|tool| tool.id.len() + tool.name.len() + tool.arguments.len())
        .sum();
    if total > MAX_TOOL_BYTES {
        return Err(upstream("Streaming tool state exceeds its total limit."));
    }
    Ok(())
}
fn sse(kind: &str, value: Value) -> Vec<u8> {
    format!("event: {kind}\ndata: {value}\n\n").into_bytes()
}

pub struct AnthropicStream {
    model: String,
    started: bool,
    terminal: bool,
    next_block: u64,
    active: Option<(u64, &'static str)>,
    tools: BTreeMap<u64, ToolFragment>,
    finish_reason: Option<String>,
    output_tokens: Option<u64>,
}

impl AnthropicStream {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            started: false,
            terminal: false,
            next_block: 0,
            active: None,
            tools: BTreeMap::new(),
            finish_reason: None,
            output_tokens: None,
        }
    }
    fn start(&mut self, input: &Value, output: &mut Vec<u8>) {
        if !self.started {
            self.started = true;
            output.extend(sse("message_start",json!({"type":"message_start","message":{"id":input.get("id").cloned().unwrap_or(Value::Null),"type":"message","role":"assistant","model":self.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":{"input_tokens":input.pointer("/usage/prompt_tokens").cloned().unwrap_or(Value::Null),"output_tokens":0}}})));
        }
    }
    fn close(&mut self, output: &mut Vec<u8>) {
        if let Some((index, _)) = self.active.take() {
            output.extend(sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
            ));
        }
    }
    fn text(&mut self, kind: &'static str, value: &str, output: &mut Vec<u8>) {
        if value.is_empty() {
            return;
        }
        if self.active.is_none_or(|(_, active)| active != kind) {
            self.close(output);
            let index = self.next_block;
            self.next_block += 1;
            self.active = Some((index, kind));
            let block = if kind == "thinking" {
                json!({"type":"thinking","thinking":""})
            } else {
                json!({"type":"text","text":""})
            };
            output.extend(sse(
                "content_block_start",
                json!({"type":"content_block_start","index":index,"content_block":block}),
            ));
        }
        let index = self.active.unwrap().0;
        let delta = if kind == "thinking" {
            json!({"type":"thinking_delta","thinking":value})
        } else {
            json!({"type":"text_delta","text":value})
        };
        output.extend(sse(
            "content_block_delta",
            json!({"type":"content_block_delta","index":index,"delta":delta}),
        ));
    }
    pub fn event(&mut self, event: SseEvent) -> Result<Vec<u8>, CodecError> {
        if self.terminal {
            return Err(upstream("Data arrived after the terminal stream event."));
        }
        if event.data == "[DONE]" {
            return self.finish();
        }
        let input: Value = serde_json::from_str(&event.data)
            .map_err(|_| upstream("Invalid JSON in worker SSE."))?;
        if event.event.as_deref() == Some("error") || input.get("error").is_some() {
            return Err(upstream("The worker stream reported an error."));
        }
        let mut output = Vec::new();
        self.start(&input, &mut output);
        if let Some(tokens) = input.pointer("/usage/completion_tokens") {
            self.output_tokens = Some(
                tokens
                    .as_u64()
                    .ok_or_else(|| upstream("Invalid streaming token usage."))?,
            );
        }
        let Some(choices) = input.get("choices").and_then(Value::as_array) else {
            return Err(upstream("Streaming choices are unavailable."));
        };
        if choices.is_empty() {
            return Ok(output);
        }
        if choices.len() != 1 {
            return Err(upstream("Only one streaming choice is supported."));
        }
        let choice = &choices[0];
        let delta = choice
            .get("delta")
            .ok_or_else(|| upstream("Streaming delta is unavailable."))?;
        for (field, kind) in [("reasoning_content", "thinking"), ("content", "text")] {
            if let Some(value) = delta.get(field).filter(|v| !v.is_null()) {
                self.text(
                    kind,
                    value
                        .as_str()
                        .ok_or_else(|| upstream("Streaming content must be text."))?,
                    &mut output,
                );
            }
        }
        accumulate_tools(delta, &mut self.tools)?;
        if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
            let reason = reason
                .as_str()
                .ok_or_else(|| upstream("Invalid finish reason."))?;
            stop_reason(Some(reason))?;
            self.finish_reason = Some(reason.to_owned());
        }
        Ok(output)
    }
    pub fn finish(&mut self) -> Result<Vec<u8>, CodecError> {
        if self.terminal {
            return Ok(Vec::new());
        }
        if !self.started || self.finish_reason.is_none() {
            return Err(upstream("Worker stream ended without a finish reason."));
        }
        let mut output = Vec::new();
        self.close(&mut output);
        for tool in self.tools.values() {
            if tool.id.is_empty() || tool.name.is_empty() {
                return Err(upstream("Incomplete streaming tool identity."));
            }
            let args: Value = serde_json::from_str(&tool.arguments)
                .map_err(|_| upstream("Incomplete streaming tool arguments."))?;
            if !args.is_object() {
                return Err(upstream("Streaming tool arguments must be an object."));
            }
            let index = self.next_block;
            self.next_block += 1;
            output.extend(sse("content_block_start",json!({"type":"content_block_start","index":index,"content_block":{"type":"tool_use","id":tool.id,"name":tool.name,"input":{}}})));
            output.extend(sse("content_block_delta",json!({"type":"content_block_delta","index":index,"delta":{"type":"input_json_delta","partial_json":tool.arguments}})));
            output.extend(sse(
                "content_block_stop",
                json!({"type":"content_block_stop","index":index}),
            ));
        }
        output.extend(sse("message_delta",json!({"type":"message_delta","delta":{"stop_reason":if self.tools.is_empty(){stop_reason(self.finish_reason.as_deref())?}else{"tool_use"},"stop_sequence":null},"usage":{"output_tokens":self.output_tokens}})));
        output.extend(sse("message_stop", json!({"type":"message_stop"})));
        self.terminal = true;
        Ok(output)
    }
}

pub struct OllamaStream {
    model: String,
    created_at: String,
    operation: OllamaOperation,
    tools: BTreeMap<u64, ToolFragment>,
    terminal: bool,
    finish_reason: Option<String>,
    usage: (Option<u64>, Option<u64>),
}
impl OllamaStream {
    pub fn new(
        model: impl Into<String>,
        created_at: impl Into<String>,
        operation: OllamaOperation,
    ) -> Result<Self, CodecError> {
        if ![OllamaOperation::Chat, OllamaOperation::Generate].contains(&operation) {
            return Err(invalid("Embedding operations do not stream."));
        }
        Ok(Self {
            model: model.into(),
            created_at: created_at.into(),
            operation,
            tools: BTreeMap::new(),
            terminal: false,
            finish_reason: None,
            usage: (None, None),
        })
    }
    pub fn event(&mut self, event: SseEvent) -> Result<Vec<u8>, CodecError> {
        if self.terminal {
            return Err(upstream("Data arrived after the terminal stream event."));
        }
        if event.data == "[DONE]" {
            return self.finish();
        }
        let input: Value = serde_json::from_str(&event.data)
            .map_err(|_| upstream("Invalid JSON in worker SSE."))?;
        if input.get("error").is_some() || event.event.as_deref() == Some("error") {
            return Err(upstream("The worker stream reported an error."));
        }
        let usage = token_usage(&input)?;
        if usage.0.is_some() {
            self.usage.0 = usage.0;
        }
        if usage.1.is_some() {
            self.usage.1 = usage.1;
        }
        let choices = input
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| upstream("Streaming choices are unavailable."))?;
        if choices.is_empty() {
            return Ok(Vec::new());
        }
        if choices.len() != 1 {
            return Err(upstream("Only one streaming choice is supported."));
        }
        let choice = &choices[0];
        let mut result = json!({"model":self.model,"created_at":self.created_at,"done":false});
        if self.operation == OllamaOperation::Chat {
            let delta = &choice["delta"];
            accumulate_tools(delta, &mut self.tools)?;
            let mut message = ollama_message(
                &json!({"content":delta.get("content").cloned().unwrap_or(Value::Null),"reasoning_content":delta.get("reasoning_content").cloned().unwrap_or(json!(""))}),
            )?;
            if message["thinking"] == "" {
                message.as_object_mut().unwrap().remove("thinking");
            }
            if message["content"] == "" && message.get("thinking").is_none() {
                result = Value::Null;
            } else {
                result["message"] = message;
            }
        } else {
            result["response"] = json!(
                choice
                    .get("text")
                    .and_then(Value::as_str)
                    .ok_or_else(|| upstream("Streaming completion text is unavailable."))?
            );
        }
        if let Some(reason) = choice.get("finish_reason").filter(|v| !v.is_null()) {
            self.finish_reason = Some(
                reason
                    .as_str()
                    .ok_or_else(|| upstream("Invalid finish reason."))?
                    .to_owned(),
            );
        }
        if result.is_null() {
            Ok(Vec::new())
        } else {
            let mut output = serde_json::to_vec(&result).unwrap();
            output.push(b'\n');
            Ok(output)
        }
    }
    pub fn finish(&mut self) -> Result<Vec<u8>, CodecError> {
        if self.terminal {
            return Ok(Vec::new());
        }
        let reason = self
            .finish_reason
            .as_deref()
            .ok_or_else(|| upstream("Worker stream ended without a finish reason."))?;
        let mut output = json!({"model":self.model,"created_at":self.created_at,"done":true,"done_reason":reason});
        if self.operation == OllamaOperation::Chat {
            let mut calls = Vec::new();
            for tool in self.tools.values() {
                let arguments: Value = serde_json::from_str(&tool.arguments)
                    .map_err(|_| upstream("Incomplete streaming tool arguments."))?;
                if tool.name.is_empty() || !arguments.is_object() {
                    return Err(upstream("Invalid streaming tool call."));
                }
                calls.push(json!({"function":{"name":tool.name,"arguments":arguments}}));
            }
            let mut message = json!({"role":"assistant","content":""});
            if !calls.is_empty() {
                message["tool_calls"] = Value::Array(calls);
            }
            output["message"] = message;
        } else {
            output["response"] = json!("");
        }
        if let Some(count) = self.usage.0 {
            output["prompt_eval_count"] = json!(count);
        }
        if let Some(count) = self.usage.1 {
            output["eval_count"] = json!(count);
        }
        self.terminal = true;
        let mut bytes = serde_json::to_vec(&output).unwrap();
        bytes.push(b'\n');
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anthropic() -> Value {
        json!({"model":"tiny","max_tokens":16,"messages":[{"role":"user","content":"hello"}]})
    }
    fn chunk(delta: Value, reason: Value) -> SseEvent {
        SseEvent {event:None,data:json!({"id":"c1","model":"tiny","choices":[{"index":0,"delta":delta,"finish_reason":reason}]}).to_string()}
    }

    #[test]
    fn anthropic_text_images_tools_and_results_preserve_meaning() {
        let request = json!({"model":"tiny","max_tokens":32,"system":[{"type":"text","text":"system"}],"messages":[{"role":"assistant","content":[{"type":"tool_use","id":"call1","name":"weather","input":{"city":"Paris"}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call1","content":[{"type":"text","text":"sunny"}]},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aGVsbG8="}}]}],"tools":[{"name":"weather","input_schema":{"type":"object"}}],"tool_choice":{"type":"any","disable_parallel_tool_use":true}});
        let converted = anthropic_request(request, false).unwrap();
        assert_eq!(converted.operation, "chat/completions");
        assert_eq!(converted.body["messages"][0]["role"], "system");
        assert_eq!(
            converted.body["messages"][1]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Paris\"}"
        );
        assert_eq!(converted.body["messages"][2]["role"], "tool");
        assert_eq!(converted.body["messages"][2]["content"], "sunny");
        assert_eq!(converted.body["messages"][3]["role"], "user");
        assert!(
            converted.body["messages"][3]["content"][0]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
        assert_eq!(converted.body["tool_choice"], "required");
        assert_eq!(converted.body["parallel_tool_calls"], false);
    }
    #[test]
    fn incompatible_fields_fail_explicitly_and_streams_request_native_usage() {
        let mut request = anthropic();
        request["stream"] = json!(true);
        assert_eq!(
            anthropic_request(request, false).unwrap().body["stream_options"]["include_usage"],
            true
        );
        let mut request = anthropic();
        request["tool_choice"] = json!({"type":"auto","name":"ignored"});
        assert!(anthropic_request(request, false).is_err());
        let mut request = anthropic();
        request["messages"][0]["content"] =
            json!([{"type":"tool_result","tool_use_id":"call","is_error":true,"content":"failed"}]);
        assert_eq!(
            anthropic_request(request, false).unwrap_err().code,
            "unsupported_parameter"
        );
        assert!(
            ollama_request(
                json!({"model":"tiny","prompt":"a","input":"b"}),
                OllamaOperation::LegacyEmbed
            )
            .is_err()
        );
        assert!(matches!(
            mcp_request(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":[]})),
            McpDispatch::Notification
        ));
    }

    #[test]
    fn anthropic_rejects_ignored_or_host_executed_semantics() {
        for (field, value) in [
            ("metadata", json!({"user_id":"private"})),
            ("context_management", json!({})),
            ("service_tier", json!("priority")),
        ] {
            let mut request = anthropic();
            request[field] = value;
            assert_eq!(
                anthropic_request(request, false).unwrap_err().code,
                "unsupported_parameter"
            );
        }
        let mut request = anthropic();
        request["tools"] = json!([{"type":"web_search_20250305","name":"web_search"}]);
        assert!(anthropic_request(request, false).is_err());
        let mut request = anthropic();
        request["thinking"] = json!({"type":"enabled","budget_tokens":100});
        assert!(anthropic_request(request, false).is_err());
    }
    #[test]
    fn native_token_count_never_approximates() {
        let mut request = anthropic();
        request.as_object_mut().unwrap().remove("max_tokens");
        let original = request.clone();
        let converted = anthropic_request(request, true).unwrap();
        assert_eq!(converted.operation, "messages/count_tokens");
        assert_eq!(converted.body, original);
        assert!(anthropic_token_count(&json!({"tokens":[1,2]})).is_err());
        assert_eq!(
            anthropic_token_count(&json!({"input_tokens":7})).unwrap(),
            json!({"input_tokens":7})
        );
    }
    #[test]
    fn anthropic_response_retains_tools_thinking_usage_and_stop() {
        let response = json!({"id":"x","model":"tiny","choices":[{"message":{"content":"","reasoning_content":"reason","tool_calls":[{"id":"t","function":{"name":"lookup","arguments":"{\"x\":1}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":8,"completion_tokens":2}});
        let converted = anthropic_response(&response).unwrap();
        assert_eq!(converted["content"][0]["type"], "thinking");
        assert_eq!(converted["content"][1]["input"]["x"], 1);
        assert_eq!(converted["stop_reason"], "tool_use");
        assert_eq!(converted["usage"]["input_tokens"], 8);
    }
    #[test]
    fn malformed_tool_arguments_and_multiple_choices_do_not_fake_success() {
        let response = json!({"choices":[{"message":{"tool_calls":[{"id":"t","function":{"name":"lookup","arguments":"broken"}}]}}]});
        assert_eq!(anthropic_response(&response).unwrap_err().status, 502);
        assert!(anthropic_response(&json!({"choices":[]})).is_err());
        assert!(anthropic_response(&json!({"choices":[{},{}]})).is_err());
    }
    #[test]
    fn ollama_chat_and_raw_generate_map_supported_options_only() {
        let chat=ollama_request(json!({"model":"tiny","messages":[{"role":"user","content":"hello","images":["AQID"]}],"options":{"num_predict":8,"temperature":0},"format":"json"}),OllamaOperation::Chat).unwrap();
        assert!(chat.stream);
        assert_eq!(chat.body["max_tokens"], 8);
        assert_eq!(
            chat.body["messages"][0]["content"][1]["image_url"]["url"],
            "data:image/png;base64,AQID"
        );
        let generate = ollama_request(
            json!({"model":"tiny","prompt":"abc","raw":true,"stream":false}),
            OllamaOperation::Generate,
        )
        .unwrap();
        assert_eq!(generate.operation, "completions");
        assert!(!generate.stream);
        assert!(
            ollama_request(
                json!({"model":"tiny","prompt":"abc","system":"must not drop"}),
                OllamaOperation::Generate
            )
            .is_err()
        );
        assert!(
            ollama_request(
                json!({"model":"tiny","prompt":"abc","options":{"num_ctx":2048}}),
                OllamaOperation::Generate
            )
            .is_err()
        );
    }
    #[test]
    fn ollama_embeddings_and_discovery_do_not_invent_measurements() {
        let request = ollama_request(
            json!({"model":"e","input":["a","b"]}),
            OllamaOperation::Embed,
        )
        .unwrap();
        assert_eq!(request.operation, "embeddings");
        let result = ollama_response(
            &json!({"data":[{"embedding":[0.1,0.2]}]}),
            OllamaOperation::Embed,
            "e",
            "now",
        )
        .unwrap();
        assert!(result.get("total_duration").is_none());
        assert!(result.get("prompt_eval_count").is_none());
        let models = ollama_models(
            &[
                json!({"id":"off","policy":"disabled","downloaded":true}),
                json!({"id":"on","policy":"hot","downloaded":true,"loaded":true}),
            ],
            false,
        );
        assert_eq!(models["models"].as_array().unwrap().len(), 1);
        assert!(models["models"][0].get("digest").is_none());
        assert!(ollama_show(&json!({"id":"off","policy":"disabled"})).is_err());
    }
    #[test]
    fn ollama_response_rehydrates_tool_arguments() {
        let output=ollama_response(&json!({"choices":[{"message":{"content":null,"tool_calls":[{"function":{"name":"read","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}),OllamaOperation::Chat,"tiny","2026-11-10T00:00:00Z").unwrap();
        assert_eq!(
            output["message"]["tool_calls"][0]["function"]["arguments"],
            json!({})
        );
        assert_eq!(output["done"], true);
    }
    #[test]
    fn mcp_lists_only_inference_tools_and_requires_explicit_model() {
        let tools = mcp_tools();
        let names: Vec<_> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(
            !names
                .iter()
                .any(|name| name.contains("omni") || name.contains("shell"))
        );
        let request = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"lemonade_chat","arguments":{"model":"tiny","messages":[{"role":"user","content":"hello"}]}}});
        assert!(
            matches!(mcp_request(request),McpDispatch::Inference{request,..} if !request.stream)
        );
        let bad = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"lemonade_chat","arguments":{"messages":[],"allow_download":true}}});
        assert!(
            matches!(mcp_request(bad),McpDispatch::Reply(value) if value["error"]["code"]==-32602)
        );
    }
    #[test]
    fn mcp_notifications_do_not_execute_and_cancellation_is_explicit() {
        assert!(matches!(
            mcp_request(
                json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"lemonade_chat"}})
            ),
            McpDispatch::Notification
        ));
        assert!(
            matches!(mcp_request(json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"r1"}})),McpDispatch::Cancel{request_id} if request_id=="r1")
        );
        assert_eq!(mcp_decode(b"[").unwrap_err()["error"]["code"], -32700);
        assert!(mcp_decode(b"[]").is_err());
        assert!(mcp_decode(br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).is_err());
    }
    #[test]
    fn mcp_initialize_and_unknown_tools_are_correctly_shaped() {
        assert!(
            matches!(mcp_request(json!({"jsonrpc":"2.0","id":"i","method":"initialize","params":{}})),McpDispatch::Reply(value) if value["result"]["protocolVersion"]=="2025-06-18")
        );
        assert!(
            matches!(mcp_request(json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"shell","arguments":{}}})),McpDispatch::Reply(value) if value["result"]["isError"]==true)
        );
        assert!(
            matches!(mcp_request(json!({"jsonrpc":"2.0","id":1.5,"method":"ping"})),McpDispatch::Reply(value) if value["error"]["code"]==-32600)
        );
    }
    #[test]
    fn sse_decoder_handles_every_utf8_byte_split_crlf_comments_and_multiline() {
        let wire =
            ": keepalive\r\nevent: message\r\ndata: {\"text\":\"héllo\",\r\ndata: \"n\":1}\r\n\r\n"
                .as_bytes();
        for split in 0..=wire.len() {
            let mut decoder = SseDecoder::default();
            let mut events = decoder.push(&wire[..split]).unwrap();
            events.extend(decoder.push(&wire[split..]).unwrap());
            events.extend(decoder.finish().unwrap());
            assert_eq!(events.len(), 1);
            assert_eq!(events[0].event.as_deref(), Some("message"));
            assert_eq!(
                serde_json::from_str::<Value>(&events[0].data).unwrap()["text"],
                "héllo"
            );
        }
    }
    #[test]
    fn sse_bounds_bad_utf8_and_many_events_in_one_chunk() {
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(&vec![b'x'; MAX_FRAME_BYTES + 1]).is_err());
        let mut decoder = SseDecoder::default();
        assert!(decoder.push(b"data: \xff\n\n").is_err());
        let mut decoder = SseDecoder::default();
        let wire = "data: {}\n\n".repeat(MAX_FRAME_BYTES / 5);
        assert_eq!(
            decoder.push(wire.as_bytes()).unwrap().len(),
            MAX_FRAME_BYTES / 5
        );
    }
    #[test]
    fn anthropic_stream_orders_content_tools_and_terminal_events() {
        let mut encoder = AnthropicStream::new("tiny");
        let mut output = encoder
            .event(chunk(json!({"reasoning_content":"r"}), Value::Null))
            .unwrap();
        output.extend(
            encoder
                .event(chunk(json!({"content":"hello"}), Value::Null))
                .unwrap(),
        );
        output.extend(encoder.event(chunk(json!({"tool_calls":[{"index":0,"id":"t","function":{"name":"lookup","arguments":"{\"x\":"}}]}),Value::Null)).unwrap());
        output.extend(
            encoder
                .event(chunk(
                    json!({"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]}),
                    json!("tool_calls"),
                ))
                .unwrap(),
        );
        output.extend(
            encoder
                .event(SseEvent {
                    event: None,
                    data: "[DONE]".into(),
                })
                .unwrap(),
        );
        let text = String::from_utf8(output).unwrap();
        assert!(text.starts_with("event: message_start"));
        assert!(text.contains("thinking_delta"));
        assert!(text.contains("input_json_delta"));
        assert!(text.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
        assert!(encoder.finish().unwrap().is_empty());
    }
    #[test]
    fn stream_truncation_and_worker_errors_are_not_success() {
        let mut encoder = AnthropicStream::new("tiny");
        encoder
            .event(chunk(json!({"content":"part"}), Value::Null))
            .unwrap();
        assert!(encoder.finish().is_err());
        let mut encoder = AnthropicStream::new("tiny");
        assert!(
            encoder
                .event(SseEvent {
                    event: Some("error".into()),
                    data: json!({"error":{"message":"private stack"}}).to_string()
                })
                .is_err()
        );
    }
    #[test]
    fn ollama_stream_emits_ndjson_and_one_terminal_event() {
        let mut encoder = OllamaStream::new("tiny", "now", OllamaOperation::Chat).unwrap();
        let mut bytes = encoder
            .event(chunk(
                json!({"content":"hello","reasoning_content":"thought"}),
                Value::Null,
            ))
            .unwrap();
        bytes.extend(encoder.event(chunk(json!({}), json!("stop"))).unwrap());
        bytes.extend(
            encoder
                .event(SseEvent {
                    event: None,
                    data: "[DONE]".into(),
                })
                .unwrap(),
        );
        let text = String::from_utf8(bytes).unwrap();
        let values: Vec<Value> = text
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["message"]["thinking"], "thought");
        assert_eq!(values[1]["done"], true);
        assert!(encoder.finish().unwrap().is_empty());
    }
}
