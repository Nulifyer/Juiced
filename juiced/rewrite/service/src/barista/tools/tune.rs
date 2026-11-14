//! Tune: measures a llama.cpp model's launch choices on this host through the normal settings
//! and chat routes, keeps a choice only when it is clearly faster, and always restores the
//! operator's saved settings. The winners become the model's tuned launch layer.

use super::check::{thinking_switch, with_switch};
use super::{Context, Failure, Result};
use serde_json::{Value, json};
use std::sync::Arc;

/// Reports progress as `{step, candidates}` while Tune runs.
pub(crate) type Progress = Arc<dyn Fn(Value) + Send + Sync>;

const CODE: &str = r#"from dataclasses import dataclass
from math import pi, sqrt


@dataclass
class Circle:
    radius: float

    def area(self) -> float:
        return pi * self.radius ** 2

    def perimeter(self) -> float:
        return 2 * pi * self.radius


@dataclass
class Rectangle:
    width: float
    height: float

    def area(self) -> float:
        return self.width * self.height

    def perimeter(self) -> float:
        return 2 * (self.width + self.height)

    def diagonal(self) -> float:
        return sqrt(self.width ** 2 + self.height ** 2)


@dataclass
class Triangle:
    a: float
    b: float
    c: float

    def area(self) -> float:
        s = (self.a + self.b + self.c) / 2
        return sqrt(s * (s - self.a) * (s - self.b) * (s - self.c))

    def perimeter(self) -> float:
        return self.a + self.b + self.c


def total_area(shapes) -> float:
    return sum(shape.area() for shape in shapes)


def largest(shapes):
    return max(shapes, key=lambda shape: shape.area())
"#;

#[derive(Clone, Copy, Debug, Default)]
struct Speed {
    prefill: f64,
    decode: f64,
    rewrite: f64,
}

impl Speed {
    fn json(&self) -> Value {
        let round = |value: f64| (value * 10.0).round() / 10.0;
        json!({"prefill_tps": round(self.prefill), "decode_tps": round(self.decode), "rewrite_tps": round(self.rewrite)})
    }
}

struct Run<'c, 'a> {
    context: &'c Context<'a>,
    id: String,
    switch: Option<(String, Value, Value)>,
    saved: Value,
    extra: Vec<String>,
    candidates: Vec<Value>,
    progress: Progress,
}

fn tps(reply: &Value, key: &str) -> f64 {
    reply["timings"][key].as_f64().unwrap_or(0.0)
}

impl Run<'_, '_> {
    fn report(&self, step: &str) {
        (self.progress)(json!({"step": step, "candidates": self.candidates}));
    }

    async fn revision(&self) -> Result<u64> {
        let options = self
            .context
            .get(&format!("models/{}/options", self.id))
            .await?;
        options["revision"].as_u64().ok_or_else(|| {
            Failure::new(
                "tune_options_unreadable",
                "The model settings have no revision.",
                502,
            )
        })
    }

    async fn save(&self, body: Value) -> Result<()> {
        let mut body = body;
        body["expected_revision"] = json!(self.revision().await?);
        self.context
            .post(&format!("models/{}/options", self.id), body)
            .await
            .map(|_| ())
    }

    async fn chat(&self, content: String, max_tokens: u32) -> Result<Value> {
        let body = with_switch(
            json!({"model": self.id, "max_tokens": max_tokens, "stream": false,
                   "messages": [{"role": "user", "content": content}]}),
            &self.switch,
            false,
        );
        self.context.post("chat/completions", body).await
    }

    /// Launches with these settings and measures prompt, chat and code-rewrite speed.
    async fn measure(&mut self, label: &str, options: Value, extra: Vec<String>) -> Result<Speed> {
        self.report(&format!("Measuring {label}"));
        self.save(json!({"options": options, "extra_args": extra, "apply": "reload"}))
            .await?;
        self.chat("Reply with OK.".into(), 4).await?;
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| time.as_nanos());
        let prompt = format!(
            "[{label} {nonce}]\n{}\nIn one sentence, which class has a diagonal method?",
            CODE.repeat(8)
        );
        let prefill = tps(&self.chat(prompt, 8).await?, "prompt_per_second");
        let decode = tps(
            &self
                .chat(format!("[{nonce}] Write the numbers from 1 to 150 separated by spaces. Output only the numbers."), 320)
                .await?,
            "predicted_per_second",
        );
        let rewrite = tps(
            &self
                .chat(
                    format!("[{nonce}] Return this Python file unchanged except rename the method `perimeter` to `circumference` everywhere. Output only the code.\n\n```python\n{CODE}```"),
                    1200,
                )
                .await?,
            "predicted_per_second",
        );
        let speed = Speed {
            prefill,
            decode,
            rewrite,
        };
        self.candidates
            .push(json!({"label": label, "speed": speed.json()}));
        self.report(&format!("Measured {label}"));
        Ok(speed)
    }
}

fn with(options: &Value, key: &str, value: Value) -> Value {
    let mut options = options.clone();
    options[key] = value;
    options
}

/// Runs Tune for `id` and returns `{arguments, measurements}`; the caller saves the result.
pub(super) async fn run(context: &Context<'_>, id: &str, progress: Progress) -> Result<Value> {
    let model = context.model(id).await?;
    let options = context.get(&format!("models/{id}/options")).await?;
    if options["provider"] != "llama-cpp" || options["model_defaults"]["available"] != true {
        return Err(Failure::new(
            "tune_unsupported",
            "Tune measures llama.cpp models with their own settings; this model's engine or settings store is not supported.",
            409,
        ));
    }
    if model["policy"] == "disabled" {
        return Err(Failure::new(
            "model_disabled",
            format!("{id} is Disabled. Set it to On demand or Hot to tune it."),
            409,
        ));
    }
    let was_loaded = model["loaded"] == true;
    let previous_tuned = options["tuned"].clone();
    let saved = options["saved"].clone();
    let extra: Vec<String> =
        serde_json::from_value(options["extra_args"].clone()).unwrap_or_default();
    let mut run = Run {
        context,
        id: id.to_owned(),
        switch: thinking_switch(&model),
        saved: saved.clone(),
        extra: extra.clone(),
        candidates: Vec::new(),
        progress,
    };
    if !previous_tuned.is_null() {
        run.save(json!({"clear_tuned": true})).await?;
    }
    let outcome = search(&mut run, &options).await;
    let restore = match &outcome {
        Ok(result) if !result["arguments"].as_array().is_none_or(Vec::is_empty) => json!({
            "options": run.saved, "extra_args": run.extra,
            "tuned": {"arguments": result["arguments"], "runtime_package": model["runtime_package"], "measured_at_ms": now_ms(), "measurements": result["measurements"]},
        }),
        Ok(_) => json!({"options": run.saved, "extra_args": run.extra, "clear_tuned": true}),
        Err(_) if previous_tuned.is_object() => {
            json!({"options": run.saved, "extra_args": run.extra, "tuned": previous_tuned})
        }
        Err(_) => json!({"options": run.saved, "extra_args": run.extra}),
    };
    let mut restore = restore;
    run.report("Restoring the saved settings");
    if was_loaded {
        restore["apply"] = json!("reload");
    } else {
        // Settings change only on an unloaded worker unless the save reloads it.
        let _ = context.post("unload", json!({"model_name": id})).await;
    }
    let restored = run.save(restore).await;
    let result = outcome?;
    restored?;
    Ok(result)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as u64)
}

/// Greedy search: physical batch, then flash attention, then speculative decoding, each kept
/// only with a clear gain and no meaningful loss.
async fn search(run: &mut Run<'_, '_>, options: &Value) -> Result<Value> {
    let saved = run.saved.clone();
    let extra = run.extra.clone();
    let base = run
        .measure("current settings", saved.clone(), extra.clone())
        .await?;
    let mut best = base;
    let mut chosen = saved.clone();
    let mut arguments: Vec<String> = Vec::new();

    if saved["ubatch_size"].is_null() {
        let mut winner: Option<(u64, Speed)> = None;
        for size in [1024u64, 2048] {
            let speed = run
                .measure(
                    &format!("physical batch {size}"),
                    with(&saved, "ubatch_size", json!(size)),
                    extra.clone(),
                )
                .await?;
            let better = speed.prefill >= best.prefill * 1.08 && speed.decode >= best.decode * 0.97;
            if better && winner.is_none_or(|(_, held)| speed.prefill > held.prefill) {
                winner = Some((size, speed));
            }
        }
        if let Some((size, speed)) = winner {
            chosen = with(&saved, "ubatch_size", json!(size));
            arguments.extend(["--ubatch-size".to_owned(), size.to_string()]);
            best = speed;
        }
    }

    if saved["flash_attention"].is_null() {
        let speed = run
            .measure(
                "flash attention on",
                with(&chosen, "flash_attention", json!("on")),
                extra.clone(),
            )
            .await?;
        let gain = speed.prefill >= best.prefill * 1.05 || speed.decode >= best.decode * 1.03;
        let loss = speed.prefill < best.prefill * 0.97 || speed.decode < best.decode * 0.97;
        if gain && !loss {
            chosen = with(&chosen, "flash_attention", json!("on"));
            arguments.extend(["--flash-attn".to_owned(), "on".to_owned()]);
            best = speed;
        }
    }

    let saved_speculation = extra.iter().any(|argument| argument == "--spec-type");
    if !saved_speculation {
        let current = options["model_defaults"]["entries"]
            .as_array()
            .and_then(|entries| {
                entries
                    .iter()
                    .find(|entry| entry["flag"] == "--spec-type" && entry["state"] == "applied")
            })
            .and_then(|entry| entry["value"].as_str())
            .unwrap_or("none")
            .to_owned();
        let alternative = if current == "none" {
            "ngram-simple"
        } else {
            "none"
        };
        let mut trial = extra.clone();
        trial.extend(["--spec-type".to_owned(), alternative.to_owned()]);
        let speed = run
            .measure(
                &format!("speculative decoding {alternative}"),
                chosen.clone(),
                trial,
            )
            .await?;
        if speed.decode * speed.rewrite >= best.decode * best.rewrite * 1.10 {
            arguments.extend(["--spec-type".to_owned(), alternative.to_owned()]);
            best = speed;
        }
    }
    Ok(json!({
        "arguments": arguments,
        "measurements": {"baseline": base.json(), "chosen": best.json(), "candidates": run.candidates},
    }))
}
