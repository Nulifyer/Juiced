//! The model check: a few bounded real requests through the normal API that show whether a
//! model's declared features work once it runs (chat, tool-call parsing, the thinking switch,
//! image input) and how fast it decodes.

use super::{Context, Failure, Result};
use serde_json::{Value, json};
use std::time::Instant;

/// A 16x16 solid red PNG.
const RED_PNG: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAABAAAAAQCAIAAACQkWg2AAAAFklEQVR42mP4z8BAEmIY1TCqYfhqAACQ+f8B8u7oVwAAAABJRU5ErkJggg==";

const WEATHER_TOOL: &str = r#"{"type":"function","function":{"name":"get_weather","description":"Current weather for a city.","parameters":{"type":"object","properties":{"city":{"type":"string"}},"required":["city"]}}}"#;

fn step(id: &str, label: &str, passed: bool, detail: impl Into<String>, started: Instant) -> Value {
    json!({"id": id, "label": label, "status": if passed {"pass"} else {"fail"}, "detail": detail.into(), "ms": started.elapsed().as_millis() as u64})
}

fn skipped(id: &str, label: &str, detail: &str) -> Value {
    json!({"id": id, "label": label, "status": "skip", "detail": detail})
}

fn failed(id: &str, label: &str, failure: &Failure, started: Instant) -> Value {
    let detail = match failure {
        Failure::InvalidParams(message) => message.clone(),
        Failure::Route(body) => body["error"]["message"]
            .as_str()
            .unwrap_or("The request failed.")
            .to_owned(),
    };
    step(id, label, false, detail, started)
}

fn excerpt(text: &str) -> String {
    let trimmed = text.trim();
    let mut short: String = trimmed.chars().take(80).collect();
    if short.len() < trimmed.len() {
        short.push('…');
    }
    short
}

/// The thinking switch this model takes, as `(field, off, on)`, when the catalog declares one.
pub(super) fn thinking_switch(model: &Value) -> Option<(String, Value, Value)> {
    let thinking = &model["thinking"];
    if thinking["supported"] != true {
        return None;
    }
    let control = thinking["request_control"].as_str()?;
    let values = thinking["request_values"].as_array()?;
    let off = values.first()?.clone();
    let on = values.last()?.clone();
    (off != on).then(|| (control.to_owned(), off, on))
}

pub(super) fn with_switch(
    mut body: Value,
    switch: &Option<(String, Value, Value)>,
    on: bool,
) -> Value {
    if let Some((control, off_value, on_value)) = switch {
        let value = if on {
            on_value.clone()
        } else {
            off_value.clone()
        };
        match control.split_once('.') {
            Some((outer, inner)) => body[outer][inner] = value,
            None => body[control.as_str()] = value,
        }
    }
    body
}

async fn chat_checks(context: &Context<'_>, model: &Value, id: &str) -> Vec<Value> {
    let switch = thinking_switch(model);
    let mut steps = Vec::new();

    let started = Instant::now();
    let body = with_switch(
        json!({"model": id, "max_tokens": 128, "stream": false,
               "messages": [{"role": "user", "content": "Count from 1 to 30, separated by spaces. Output only the numbers."}]}),
        &switch,
        false,
    );
    let mut decode = Value::Null;
    match context.post("chat/completions", body).await {
        Ok(reply) => {
            let content = reply["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("");
            decode = reply["timings"]["predicted_per_second"].clone();
            steps.push(step(
                "chat",
                "Chat",
                !content.trim().is_empty(),
                if content.trim().is_empty() {
                    "The reply was empty.".to_owned()
                } else {
                    excerpt(content)
                },
                started,
            ));
        }
        Err(failure) => steps.push(failed("chat", "Chat", &failure, started)),
    }

    if model["tool_calls"]["supported"] == true {
        let started = Instant::now();
        let tool: Value = serde_json::from_str(WEATHER_TOOL).unwrap_or_default();
        let body = with_switch(
            json!({"model": id, "max_tokens": 256, "stream": false, "tools": [tool],
                   "messages": [{"role": "user", "content": "What is the weather in Paris right now? Use the tool."}]}),
            &switch,
            false,
        );
        match context.post("chat/completions", body).await {
            Ok(reply) => {
                let call = &reply["choices"][0]["message"]["tool_calls"][0]["function"];
                let arguments = call["arguments"]
                    .as_str()
                    .and_then(|text| serde_json::from_str::<Value>(text).ok());
                let city = arguments.as_ref().and_then(|value| value["city"].as_str());
                let passed = call["name"] == "get_weather" && city.is_some();
                let detail = if passed {
                    format!("get_weather({})", city.unwrap_or_default())
                } else if call.is_null() {
                    "The model answered without a parsed tool call.".to_owned()
                } else {
                    "The tool call arguments did not parse as JSON with a city.".to_owned()
                };
                steps.push(step("tools", "Tool calls", passed, detail, started));
            }
            Err(failure) => steps.push(failed("tools", "Tool calls", &failure, started)),
        }
    } else {
        steps.push(skipped(
            "tools",
            "Tool calls",
            "The chat template declares no tools.",
        ));
    }

    if switch.is_some() {
        let started = Instant::now();
        let ask = |on: bool| {
            with_switch(
                json!({"model": id, "max_tokens": 384, "stream": false,
                       "messages": [{"role": "user", "content": "Is 91 a prime number? Answer yes or no."}]}),
                &switch,
                on,
            )
        };
        let reasoning = |reply: &Value| {
            reply["choices"][0]["message"]["reasoning_content"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
        };
        match (
            context.post("chat/completions", ask(true)).await,
            context.post("chat/completions", ask(false)).await,
        ) {
            (Ok(on), Ok(off)) => {
                let (thinks, quiet) = (reasoning(&on), !reasoning(&off));
                let detail = match (thinks, quiet) {
                    (true, true) => "Reasoning appears with thinking on and not with it off.",
                    (false, _) => "Thinking on returned no separate reasoning.",
                    (true, false) => "Thinking off still returned reasoning.",
                };
                steps.push(step(
                    "thinking",
                    "Thinking switch",
                    thinks && quiet,
                    detail,
                    started,
                ));
            }
            (Err(failure), _) | (_, Err(failure)) => {
                steps.push(failed("thinking", "Thinking switch", &failure, started))
            }
        }
    } else {
        steps.push(skipped(
            "thinking",
            "Thinking switch",
            "The model has no thinking switch.",
        ));
    }

    if model["vision"] == true {
        let started = Instant::now();
        let body = with_switch(
            json!({"model": id, "max_tokens": 32, "stream": false,
                   "messages": [{"role": "user", "content": [
                       {"type": "text", "text": "What color is this image? Answer with one word."},
                       {"type": "image_url", "image_url": {"url": RED_PNG}}]}]}),
            &switch,
            false,
        );
        match context.post("chat/completions", body).await {
            Ok(reply) => {
                let content = reply["choices"][0]["message"]["content"]
                    .as_str()
                    .unwrap_or("");
                let passed = content.to_ascii_lowercase().contains("red");
                steps.push(step(
                    "vision",
                    "Image input",
                    passed,
                    if passed {
                        excerpt(content)
                    } else {
                        format!("Expected red, got: {}", excerpt(content))
                    },
                    started,
                ));
            }
            Err(failure) => steps.push(failed("vision", "Image input", &failure, started)),
        }
    } else {
        steps.push(skipped(
            "vision",
            "Image input",
            "No vision projector is attached.",
        ));
    }
    if let Some(tps) = decode.as_f64() {
        steps.push(json!({"id": "speed", "label": "Decode speed", "status": "info", "detail": format!("{tps:.1} tok/s"), "tokens_per_second": tps}));
    }
    steps
}

/// Runs the check for one registered model and returns `{model, checked_at_ms, steps, passed}`.
pub(super) async fn run(context: &Context<'_>, id: &str) -> Result<Value> {
    let model = context.model(id).await?;
    if model["policy"] == "disabled" {
        return Err(Failure::new(
            "model_disabled",
            format!("{id} is Disabled. Set it to On demand or Hot to check it."),
            409,
        ));
    }
    let started = Instant::now();
    let operations: Vec<&str> = model["operations"]
        .as_array()
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let steps = if operations.contains(&"chat/completions") {
        chat_checks(context, &model, id).await
    } else {
        let label = "Request";
        match super::verify::test_once(context, id).await {
            Ok(summary) => vec![step("request", label, true, summary, started)],
            Err(failure) => vec![failed("request", label, &failure, started)],
        }
    };
    let passed = steps.iter().all(|step| step["status"] != "fail");
    let checked_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as u64);
    Ok(
        json!({"schema_version": 1, "model": id, "checked_at_ms": checked_at_ms, "seconds": started.elapsed().as_secs_f64(), "passed": passed, "steps": steps}),
    )
}
