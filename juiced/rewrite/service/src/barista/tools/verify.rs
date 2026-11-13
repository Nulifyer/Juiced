use super::*;
use base64::Engine as _;
use std::time::Instant;

const SPEECH_CHECK: &[u8] = include_bytes!("../assets/speech-check.wav");
pub(super) const BENCHMARK_PROFILE: &str = "quick-1";
const QUICK_PROMPTS: [&str; 3] = [
    "In one sentence, what does a local model server do?",
    "List three practical uses for a small language model running on a home server. Answer in a short numbered list.",
    "Write a Python function that returns the n-th Fibonacci number iteratively, with a one-line docstring.",
];

pub(super) const TOOLS: &[Tool] = &[
    Tool {
        name: "juiced_test_model",
        group: Group::Verify,
        title: "Test model",
        description: "Send one bounded real request through the normal API: chat, embed, speech, transcribe, image or rerank. Returns an excerpt, timings, speed and cache path. Uses fixed inputs unless input is given.",
        read_only: true,
        destructive: false,
        idempotent: false,
        open_world: false,
        preview: false,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["model"],"properties":{
                "model":model_property(),
                "kind":{"type":"string","enum":["chat","embed","speech","transcribe","image","rerank"],"description":"Omit to use the model's task."},
                "input":{"type":"string","maxLength":2000,"description":"Prompt, text to speak or query. Optional."},
                "voice":{"type":"string","maxLength":64}
            },"additionalProperties":false})
        },
    },
    Tool {
        name: "juiced_benchmark",
        group: Group::Verify,
        title: "Quick benchmark",
        description: "Measure one chat model with the fixed shipped prompt set: cold load, prompt and decode speed, warm repeat. Restores the prior load state. Speed only, not a quality or qualification result.",
        read_only: false,
        destructive: false,
        idempotent: false,
        open_world: false,
        preview: true,
        support: Support::Working,
        input: || {
            json!({"type":"object","required":["model"],"properties":{
                "model":model_property(),
                "profile":{"type":"string","enum":["quick"],"default":"quick"}
            },"additionalProperties":false})
        },
    },
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestArgs {
    model: String,
    kind: Option<String>,
    input: Option<String>,
    voice: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BenchmarkArgs {
    model: String,
    profile: Option<String>,
}

pub(super) async fn run(context: &Context<'_>, name: &str, args: Value) -> Option<Result<Outcome>> {
    Some(match name {
        "juiced_test_model" => test(context, args).await,
        "juiced_benchmark" => benchmark(context, args).await,
        _ => return None,
    })
}

fn excerpt(text: &str, maximum: usize) -> String {
    let text = text.trim();
    if text.len() <= maximum {
        return text.to_owned();
    }
    let mut end = maximum;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

fn default_kind(model: &Value) -> &'static str {
    match inspect::task_of(model) {
        "embeddings" => "embed",
        "tts" => "speech",
        "transcription" => "transcribe",
        "image" => "image",
        "reranking" => "rerank",
        _ => "chat",
    }
}

async fn latest_record(context: &Context<'_>, model: &str, after: u64) -> Option<Value> {
    let records = context.get("juiced/requests").await.ok()?;
    list(&records, "data")
        .into_iter()
        .rev()
        .find(|record| record["model"] == model && record["id"].as_u64().unwrap_or(0) > after)
        .map(|record| inspect::request_row(&record))
}

async fn last_record_id(context: &Context<'_>) -> u64 {
    context
        .get("juiced/requests")
        .await
        .ok()
        .and_then(|records| {
            list(&records, "data")
                .iter()
                .filter_map(|record| record["id"].as_u64())
                .max()
        })
        .unwrap_or(0)
}

fn failed(raw: &Raw) -> Failure {
    let value: Value = serde_json::from_slice(&raw.bytes).unwrap_or(Value::Null);
    let message = value["error"]["message"]
        .as_str()
        .or(value["error"].as_str())
        .unwrap_or("The test request failed.")
        .to_owned();
    let code = value["error"]["code"]
        .as_str()
        .unwrap_or("test_failed")
        .to_owned();
    Failure::new(&code, message, raw.status)
}

pub(super) async fn chat(
    context: &Context<'_>,
    model: &str,
    prompt: &str,
    max_tokens: u32,
) -> Result<(Value, u64)> {
    let started = Instant::now();
    let reply = context
        .post(
            "chat/completions",
            json!({"model":model,"messages":[{"role":"user","content":prompt}],"max_tokens":max_tokens,"stream":false}),
        )
        .await?;
    Ok((reply, started.elapsed().as_millis() as u64))
}

fn multipart(model: &str, audio: &[u8]) -> (String, Vec<u8>) {
    let boundary = format!("juiced-barista-{:x}", audio.len() ^ 0x5a5a_5a5a);
    let mut body = Vec::new();
    for (name, value) in [("model", model), ("response_format", "json")] {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!("--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"speech-check.wav\"\r\nContent-Type: audio/wav\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

async fn test(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let test: TestArgs = parse(args)?;
    model_id(&test.model)?;
    if let Some(input) = &test.input {
        bounded(input, 1, 2000, "input")?;
    }
    if let Some(voice) = &test.voice {
        bounded(voice, 1, 64, "voice")?;
    }
    let model = context.model(&test.model).await?;
    if model["policy"] == "disabled" {
        return Err(Failure::new(
            "model_disabled",
            format!(
                "{} is Disabled, so it refuses requests. Set it to on-demand or hot first.",
                test.model
            ),
            409,
        ));
    }
    if context.permission == Permission::ReadOnly && model["loaded"] != true {
        return Err(Failure::new(
            "assistant_read_only",
            format!(
                "{} is not loaded and a test would load it. Read only permission does not allow that.",
                test.model
            ),
            403,
        ));
    }
    let kind = match test.kind.as_deref() {
        Some(kind @ ("chat" | "embed" | "speech" | "transcribe" | "image" | "rerank")) => kind,
        Some(_) => {
            return Err(Failure::InvalidParams(
                "kind must be chat, embed, speech, transcribe, image or rerank.".into(),
            ));
        }
        None => default_kind(&model),
    };
    let was_loaded = model["loaded"] == true;
    let before = last_record_id(context).await;
    let started = Instant::now();
    let (summary, mut data) = match kind {
        "chat" => {
            let prompt = test
                .input
                .as_deref()
                .unwrap_or("Reply with one short sentence that says you are ready.");
            let (reply, elapsed) = chat(context, &test.model, prompt, 96).await?;
            let message = &reply["choices"][0]["message"];
            let content = message["content"].as_str().unwrap_or("");
            let reasoning = message["reasoning_content"].as_str().unwrap_or("");
            let timings = pick(
                &reply["timings"],
                &[
                    "prompt_n",
                    "prompt_ms",
                    "prompt_per_second",
                    "predicted_n",
                    "predicted_ms",
                    "predicted_per_second",
                    "cache_n",
                ],
            );
            let speed = reply["timings"]["predicted_per_second"].as_f64();
            (
                format!(
                    "{} answered in {elapsed} ms{}: \"{}\"",
                    test.model,
                    speed.map_or(String::new(), |speed| format!(" at {speed:.1} tokens/s")),
                    excerpt(
                        if content.is_empty() {
                            reasoning
                        } else {
                            content
                        },
                        160
                    )
                ),
                json!({
                    "kind": kind, "elapsed_ms": elapsed, "output": excerpt(content, 600),
                    "reasoning": (!reasoning.is_empty()).then(|| excerpt(reasoning, 300)),
                    "finish_reason": reply["choices"][0]["finish_reason"],
                    "usage": reply["usage"], "timings": timings,
                }),
            )
        }
        "embed" => {
            let input = test.input.as_deref().unwrap_or("Juiced embedding check.");
            let reply = context
                .post("embeddings", json!({"model":test.model,"input":input}))
                .await?;
            let vector = list(&reply["data"][0], "embedding");
            (
                format!(
                    "{} returned a {}-dimension embedding.",
                    test.model,
                    vector.len()
                ),
                json!({"kind": kind, "dimensions": vector.len(), "first_values": vector.iter().take(4).collect::<Vec<_>>(), "usage": reply["usage"]}),
            )
        }
        "speech" => {
            let input = test
                .input
                .as_deref()
                .unwrap_or("Juiced speech check. One, two, three.");
            let mut body = json!({"model":test.model,"input":input,"response_format":"wav"});
            if let Some(voice) = &test.voice {
                body["voice"] = json!(voice);
            }
            let raw = context.post_raw("audio/speech", body).await?;
            if !(200..300).contains(&raw.status) {
                return Err(failed(&raw));
            }
            let content_type = if raw.content_type.is_empty() {
                "audio/wav".to_owned()
            } else {
                raw.content_type.clone()
            };
            let bytes = raw.bytes.len();
            let handle = super::super::handles::store(super::super::handles::Value::Media {
                content_type: content_type.clone(),
                bytes: raw.bytes.to_vec(),
            });
            (
                format!("{} produced {} of audio.", test.model, size(bytes as u64)),
                json!({"kind": kind, "bytes": bytes, "content_type": content_type, "media": handle.map(|handle| json!({"handle": handle, "type": content_type}))}),
            )
        }
        "transcribe" => {
            let (content_type, body) = multipart(&test.model, SPEECH_CHECK);
            let raw = context
                .post_bytes("audio/transcriptions", &content_type, body)
                .await?;
            if !(200..300).contains(&raw.status) {
                return Err(failed(&raw));
            }
            let reply: Value = serde_json::from_slice(&raw.bytes).unwrap_or(Value::Null);
            let text = reply["text"].as_str().unwrap_or("");
            (
                format!(
                    "{} transcribed the shipped check clip: \"{}\"",
                    test.model,
                    excerpt(text, 160)
                ),
                json!({"kind": kind, "text": excerpt(text, 600), "input": "shipped speech-check.wav (English, 3 s)"}),
            )
        }
        "image" => {
            let prompt = test
                .input
                .as_deref()
                .unwrap_or("a small red cube on a white table, studio light");
            let reply = context
                .post("images/generations", json!({"model":test.model,"prompt":prompt,"n":1,"size":"256x256","response_format":"b64_json"}))
                .await?;
            let encoded = reply["data"][0]["b64_json"].as_str().unwrap_or("");
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(encoded)
                .unwrap_or_default();
            let handle = (!bytes.is_empty())
                .then(|| {
                    super::super::handles::store(super::super::handles::Value::Media {
                        content_type: "image/png".into(),
                        bytes: bytes.clone(),
                    })
                })
                .flatten();
            (
                format!(
                    "{} generated a {} image.",
                    test.model,
                    size(bytes.len() as u64)
                ),
                json!({"kind": kind, "bytes": bytes.len(), "media": handle.map(|handle| json!({"handle": handle, "type": "image/png"}))}),
            )
        }
        _ => {
            let query = test
                .input
                .as_deref()
                .unwrap_or("Which sentence is about coffee?");
            let documents = [
                "The cat slept on the warm windowsill.",
                "Espresso is brewed by forcing hot water through ground coffee.",
                "Rain is expected tomorrow afternoon.",
            ];
            let reply = context
                .post(
                    "rerank",
                    json!({"model":test.model,"query":query,"documents":documents}),
                )
                .await?;
            let results: Vec<Value> = list(&reply, "results")
                .iter()
                .map(|row| pick(row, &["index", "relevance_score"]))
                .collect();
            (
                format!(
                    "{} ranked {} documents; top index {}.",
                    test.model,
                    results.len(),
                    results
                        .first()
                        .map_or(json!(null), |row| row["index"].clone())
                ),
                json!({"kind": kind, "results": results, "expected_top_index": 1}),
            )
        }
    };
    data["wall_ms"] = json!(started.elapsed().as_millis() as u64);
    data["cold_start"] = json!(!was_loaded);
    if let Some(record) = latest_record(context, &test.model, before).await {
        data["record"] = record;
    }
    let mut outcome = Outcome::new(summary, data);
    if !was_loaded {
        outcome = outcome.warn(format!(
            "{} was not loaded, so this request included its load time.",
            test.model
        ));
    }
    Ok(outcome)
}

async fn benchmark(context: &Context<'_>, args: Value) -> Result<Outcome> {
    let bench: BenchmarkArgs = parse(args)?;
    model_id(&bench.model)?;
    if bench
        .profile
        .as_deref()
        .is_some_and(|profile| profile != "quick")
    {
        return Err(Failure::InvalidParams(
            "Only the quick profile exists in this build.".into(),
        ));
    }
    let model = context.model(&bench.model).await?;
    if inspect::task_of(&model) != "chat" {
        return Err(Failure::new(
            "not_supported",
            "The quick benchmark measures chat models only; use juiced_test_model for others.",
            501,
        ));
    }
    if model["policy"] == "disabled" {
        return Err(Failure::new(
            "model_disabled",
            format!("{} is Disabled. Set it to on-demand first.", bench.model),
            409,
        ));
    }
    let was_loaded = model["loaded"] == true;
    let restore = !was_loaded && model["policy"] != "hot";
    if context.preview {
        return Ok(Outcome::new(
            format!(
                "Would run the {BENCHMARK_PROFILE} prompt set ({} prompts plus one warm repeat) on {}{}.",
                QUICK_PROMPTS.len(),
                bench.model,
                if was_loaded {
                    ""
                } else {
                    ", loading it first and unloading it after"
                }
            ),
            json!({"profile": BENCHMARK_PROFILE, "prompts": QUICK_PROMPTS.len(), "loads": !was_loaded, "restores": restore, "reserves_bytes": model["resident_bytes"]}),
        ));
    }
    let mut cold_load_ms = None;
    if !was_loaded {
        let started = Instant::now();
        context.post("load", json!({"model":bench.model})).await?;
        cold_load_ms = Some(started.elapsed().as_millis() as u64);
    }
    let mut runs = Vec::new();
    let mut failure = None;
    for (index, prompt) in QUICK_PROMPTS
        .iter()
        .chain(QUICK_PROMPTS.iter().take(1))
        .enumerate()
    {
        if !context.still_authorized().await {
            failure = Some(Failure::new(
                "assistant_settings_changed",
                "Barista settings changed during the benchmark; it stopped early.",
                409,
            ));
            break;
        }
        let before = last_record_id(context).await;
        match chat(context, &bench.model, prompt, 128).await {
            Ok((reply, elapsed)) => {
                let record = latest_record(context, &bench.model, before)
                    .await
                    .unwrap_or(Value::Null);
                runs.push(json!({
                    "prompt": index % QUICK_PROMPTS.len(), "warm_repeat": index >= QUICK_PROMPTS.len(), "elapsed_ms": elapsed,
                    "prompt_tokens": reply["timings"]["prompt_n"].as_u64().or(reply["usage"]["prompt_tokens"].as_u64()),
                    "output_tokens": reply["timings"]["predicted_n"].as_u64().or(reply["usage"]["completion_tokens"].as_u64()),
                    "prompt_tokens_per_second": reply["timings"]["prompt_per_second"].as_f64().or(record["prompt_tokens_per_second"].as_f64()),
                    "decode_tokens_per_second": reply["timings"]["predicted_per_second"].as_f64().or(record["decode_tokens_per_second"].as_f64()),
                    "cached_tokens": record["cached_tokens"],
                }));
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    let mut restored = json!(was_loaded);
    if restore {
        restored = json!(
            context
                .post("unload", json!({"model":bench.model}))
                .await
                .is_ok()
        );
    }
    if let Some(failure) = failure
        && runs.is_empty()
    {
        return Err(failure);
    }
    let median = |key: &str| {
        let mut values: Vec<f64> = runs
            .iter()
            .filter(|run| run["warm_repeat"] == false)
            .filter_map(|run| run[key].as_f64())
            .collect();
        values.sort_by(|left, right| left.total_cmp(right));
        values.get(values.len() / 2).copied()
    };
    let decode = median("decode_tokens_per_second");
    let prompt = median("prompt_tokens_per_second");
    Ok(Outcome::new(
        format!(
            "{} on {BENCHMARK_PROFILE}: decode {} tokens/s, prompt {} tokens/s{}. Speed only, not a qualification.",
            bench.model,
            decode.map_or("unknown".into(), |value| format!("{value:.1}")),
            prompt.map_or("unknown".into(), |value| format!("{value:.1}")),
            cold_load_ms.map_or(String::new(), |ms| format!(", cold load {ms} ms")),
        ),
        json!({
            "profile": BENCHMARK_PROFILE, "qualification": false, "cold_load_ms": cold_load_ms,
            "median_decode_tokens_per_second": decode, "median_prompt_tokens_per_second": prompt,
            "runs": runs, "restored_prior_state": restored,
        }),
    ))
}
