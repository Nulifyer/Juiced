//! Tune: measures a llama.cpp model's launch choices on this host through the normal settings
//! and chat routes and keeps a choice only when it is clearly faster. Each trial is saved as the
//! model's tuned launch layer, so the operator's own settings are never written; the winners
//! become that layer.

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
    path: String,
    switch: Option<(String, Value, Value)>,
    runtime_package: Value,
    previous: Value,
    /// Marks this run's trials, so cleanup replaces only a trial this run saved.
    run_id: String,
    deadline: tokio::time::Instant,
    /// The settings revision Tune's own last save produced; any other change stops Tune.
    revision: Option<u64>,
    candidates: Vec<Value>,
    progress: Progress,
}

fn tps(reply: &Value, key: &str) -> f64 {
    reply["timings"][key].as_f64().unwrap_or(0.0)
}

/// A trial is saved as the tuned layer, never as the operator's settings, and carries the
/// tuned result it replaced so an interrupted run can be put back after a restart.
fn is_trial(tuned: &Value) -> bool {
    tuned["measurements"]["trial"] == true
}

/// The tuned result the operator last had, looking through a trial an interrupted run left.
fn settled(tuned: &Value) -> Value {
    if is_trial(tuned) {
        tuned["measurements"]["previous"].clone()
    } else {
        tuned.clone()
    }
}

fn restore_body(previous: &Value) -> Value {
    if previous.is_object() {
        json!({"tuned": previous, "apply": "reload"})
    } else {
        json!({"clear_tuned": true, "apply": "reload"})
    }
}

impl Run<'_, '_> {
    fn report(&self, step: &str) {
        (self.progress)(json!({"step": step, "candidates": self.candidates}));
    }

    async fn current_revision(&self) -> Result<u64> {
        let options = self.context.get(&self.path).await?;
        options["revision"].as_u64().ok_or_else(|| {
            Failure::new(
                "tune_options_unreadable",
                "The model settings have no revision.",
                502,
            )
        })
    }

    /// Saves through the settings route at Tune's own revision, waiting out a worker that is
    /// briefly busy with a client request.
    async fn save(&mut self, body: Value) -> Result<()> {
        let expected = match self.revision {
            Some(revision) => revision,
            None => self.current_revision().await?,
        };
        let mut attempt = 0;
        loop {
            let mut body = body.clone();
            body["expected_revision"] = json!(expected);
            match self.context.post(&self.path, body).await {
                Err(Failure::Route(error))
                    if error["error"]["code"] == "worker_busy" && attempt < 40 =>
                {
                    attempt += 1;
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                Err(Failure::Route(error)) if error["error"]["code"] == "revision_conflict" => {
                    return Err(Failure::new(
                        "tune_interrupted",
                        "The model settings changed while Tune ran, so Tune stopped. Your changes are kept.",
                        409,
                    ));
                }
                Err(error) => return Err(error),
                Ok(view) => {
                    self.revision = view["revision"].as_u64();
                    return Ok(());
                }
            }
        }
    }

    async fn chat(&self, content: String, max_tokens: u32) -> Result<Value> {
        let body = with_switch(
            json!({"model": self.id, "max_tokens": max_tokens, "stream": false,
                   "messages": [{"role": "user", "content": content}]}),
            &self.switch,
            false,
        );
        tokio::time::timeout_at(self.deadline, self.context.post("chat/completions", body))
            .await
            .unwrap_or_else(|_| Err(timed_out()))
    }

    /// Unloads the model, waiting out requests a client is still sending it.
    async fn unload(&self) {
        for _ in 0..40 {
            match self
                .context
                .post("unload", json!({"model_name": self.id}))
                .await
            {
                Err(Failure::Route(error)) if error["error"]["code"] == "worker_busy" => {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                }
                _ => return,
            }
        }
    }

    /// Puts back the tuned result this run replaced, unless the saved layer is no longer one
    /// of this run's trials: an operator's own change to it stays.
    async fn put_back(&mut self) -> Result<()> {
        let options = self.context.get(&self.path).await?;
        let tuned = &options["tuned"];
        if !is_trial(tuned) || tuned["measurements"]["run"] != self.run_id.as_str() {
            return Ok(());
        }
        self.revision = options["revision"].as_u64();
        let body = restore_body(&self.previous);
        self.save(body).await
    }

    /// Launches with `arguments` as the tuned layer and measures prompt, chat and code-rewrite
    /// speed.
    async fn measure(&mut self, label: &str, arguments: &[String]) -> Result<Speed> {
        if tokio::time::Instant::now() >= self.deadline {
            return Err(timed_out());
        }
        self.report(&format!("Measuring {label}"));
        let trial = json!({
            "arguments": arguments, "runtime_package": self.runtime_package, "measured_at_ms": now_ms(),
            "measurements": {"trial": true, "run": self.run_id, "previous": self.previous},
        });
        self.save(json!({"tuned": trial, "apply": "reload"}))
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
        if [prefill, decode, rewrite]
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return Err(Failure::new(
                "tune_no_timings",
                format!(
                    "The engine reported no speed for {label}, so Tune cannot compare settings."
                ),
                502,
            ));
        }
        self.candidates
            .push(json!({"label": label, "speed": speed.json()}));
        self.report(&format!("Measured {label}"));
        Ok(speed)
    }
}

/// Tune reloads the model about six times and sends a few long requests each time.
const TUNE_LIMIT: std::time::Duration = std::time::Duration::from_secs(45 * 60);

fn timed_out() -> Failure {
    Failure::new(
        "tune_timeout",
        "Tune took longer than 45 minutes and stopped.",
        504,
    )
}

fn options_path(id: &str) -> String {
    format!("models/{}/options", super::encode(id))
}

/// Runs Tune for `id` and saves its result as the model's tuned layer. Trials change only that
/// layer, so the operator's settings and extra arguments are never written.
pub(super) async fn run(context: &Context<'_>, id: &str, progress: Progress) -> Result<Value> {
    let model = context.model(id).await?;
    let path = options_path(id);
    let options = context.get(&path).await?;
    if options["provider"] != "llama-cpp" || options["model_defaults"]["available"] != true {
        return Err(Failure::new(
            "tune_unsupported",
            "Tune measures llama.cpp models with their own settings; this model's engine or settings store is not supported.",
            409,
        ));
    }
    if options["model_defaults"]["enabled"] != true {
        return Err(Failure::new(
            "tune_needs_defaults",
            format!(
                "Tune results apply through Juiced defaults, which are off for {id}. Turn them on to tune it."
            ),
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
    let mut run = Run {
        context,
        id: id.to_owned(),
        path,
        switch: thinking_switch(&model),
        runtime_package: model["runtime_package"].clone(),
        previous: settled(&options["tuned"]),
        run_id: format!("{:x}", now_ms()),
        deadline: tokio::time::Instant::now() + TUNE_LIMIT,
        revision: None,
        candidates: Vec::new(),
        progress,
    };
    let outcome = search(&mut run, &options).await;
    run.report("Saving the result");
    if !was_loaded {
        run.unload().await;
    }
    let saved = match &outcome {
        Ok(result) if !result["arguments"].as_array().is_none_or(Vec::is_empty) => {
            let body = json!({
                "tuned": {"arguments": result["arguments"], "runtime_package": run.runtime_package, "measured_at_ms": now_ms(), "measurements": result["measurements"]},
                "apply": "reload",
            });
            run.save(body).await
        }
        Ok(_) => {
            run.save(json!({"clear_tuned": true, "apply": "reload"}))
                .await
        }
        Err(_) => Ok(()),
    };
    let failure = match (outcome, saved) {
        (Ok(result), Ok(())) => return Ok(result),
        (Ok(_), Err(failure)) | (Err(failure), _) => failure,
    };
    // A failed final reload rolls back to the last trial, so cleanup follows either failure.
    match run.put_back().await {
        Ok(()) => Err(failure),
        Err(_) => {
            let error = failure.error();
            Err(Failure::new(
                error["code"].as_str().unwrap_or("tune_failed"),
                format!(
                    "{} The earlier tuned result could not be put back yet; Juiced restores it when the service next starts.",
                    error["message"].as_str().unwrap_or("Tune failed.")
                ),
                error["status"].as_u64().map_or(500, |status| status as u16),
            ))
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis() as u64)
}

fn saved_flag(options: &Value, field: Option<&str>, extra: &[String], flags: &[&str]) -> bool {
    field.is_some_and(|field| !options["saved"][field].is_null())
        || extra.iter().any(|argument| {
            let name = argument
                .split_once('=')
                .map_or(argument.as_str(), |(name, _)| name);
            flags.contains(&name)
        })
}

/// A larger physical batch must speed up the prompt clearly without slowing chat.
fn batch_kept(speed: Speed, best: Speed) -> bool {
    speed.prefill >= best.prefill * 1.08 && speed.decode >= best.decode * 0.97
}

/// Flash attention must clearly help the prompt or chat and hurt neither.
fn flash_kept(speed: Speed, best: Speed) -> bool {
    let gain = speed.prefill >= best.prefill * 1.05 || speed.decode >= best.decode * 1.03;
    let loss = speed.prefill < best.prefill * 0.97 || speed.decode < best.decode * 0.97;
    gain && !loss
}

/// Speculation trades chat for rewrite speed, so it must win on both together.
fn speculation_kept(speed: Speed, best: Speed) -> bool {
    speed.decode * speed.rewrite >= best.decode * best.rewrite * 1.10
}

/// Greedy search: physical batch, then flash attention, then speculative decoding, each kept
/// only with a clear gain and no meaningful loss. Options the operator saved are not tried.
async fn search(run: &mut Run<'_, '_>, options: &Value) -> Result<Value> {
    let extra: Vec<String> =
        serde_json::from_value(options["extra_args"].clone()).unwrap_or_default();
    let base = run.measure("current settings", &[]).await?;
    let mut best = base;
    let mut arguments: Vec<String> = Vec::new();

    if !saved_flag(
        options,
        Some("ubatch_size"),
        &extra,
        &["--ubatch-size", "-ub"],
    ) {
        let mut winner: Option<(u64, Speed)> = None;
        for size in [1024u64, 2048] {
            let trial = ["--ubatch-size".to_owned(), size.to_string()];
            let speed = run
                .measure(&format!("physical batch {size}"), &trial)
                .await?;
            if batch_kept(speed, best)
                && winner.is_none_or(|(_, held)| speed.prefill > held.prefill)
            {
                winner = Some((size, speed));
            }
        }
        if let Some((size, speed)) = winner {
            arguments.extend(["--ubatch-size".to_owned(), size.to_string()]);
            best = speed;
        }
    }

    if !saved_flag(
        options,
        Some("flash_attention"),
        &extra,
        &["--flash-attn", "-fa"],
    ) {
        let mut trial = arguments.clone();
        trial.extend(["--flash-attn".to_owned(), "on".to_owned()]);
        let speed = run.measure("flash attention on", &trial).await?;
        if flash_kept(speed, best) {
            arguments = trial;
            best = speed;
        }
    }

    if !saved_flag(options, None, &extra, &["--spec-type"]) {
        // The speculation a launch without tuning uses: a recipe's, or none.
        let current = options["model_defaults"]["entries"]
            .as_array()
            .and_then(|entries| {
                entries.iter().find(|entry| {
                    entry["flag"] == "--spec-type"
                        && entry["source"] != "tuned"
                        && (entry["state"] == "applied" || entry["state"] == "shadowed")
                })
            })
            .and_then(|entry| entry["value"].as_str())
            .unwrap_or("none")
            .to_owned();
        let alternative = if current == "none" {
            "ngram-simple"
        } else {
            "none"
        };
        let mut trial = arguments.clone();
        trial.extend(["--spec-type".to_owned(), alternative.to_owned()]);
        let speed = run
            .measure(&format!("speculative decoding {alternative}"), &trial)
            .await?;
        if speculation_kept(speed, best) {
            arguments = trial;
            best = speed;
        }
    }
    Ok(json!({
        "arguments": arguments,
        "measurements": {"baseline": base.json(), "chosen": best.json(), "candidates": run.candidates},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_interrupted_trial_restores_the_result_it_replaced() {
        let previous = json!({"arguments": ["--ubatch-size", "1024"], "measured_at_ms": 1, "measurements": {}});
        let trial = json!({"arguments": [], "measured_at_ms": 2, "measurements": {"trial": true, "previous": previous}});
        assert!(is_trial(&trial));
        assert_eq!(settled(&trial), previous);
        assert_eq!(restore_body(&settled(&trial))["tuned"], previous);
        let first = json!({"arguments": [], "measured_at_ms": 2, "measurements": {"trial": true, "previous": null}});
        assert_eq!(restore_body(&settled(&first))["clear_tuned"], true);
        assert!(!is_trial(&previous));
        assert_eq!(settled(&previous), previous);
    }

    fn speed(prefill: f64, decode: f64, rewrite: f64) -> Speed {
        Speed {
            prefill,
            decode,
            rewrite,
        }
    }

    #[test]
    fn a_choice_is_kept_only_with_a_clear_gain_and_no_real_loss() {
        let base = speed(1000.0, 40.0, 40.0);
        assert!(batch_kept(speed(1100.0, 39.0, 40.0), base));
        assert!(!batch_kept(speed(1050.0, 40.0, 40.0), base));
        assert!(!batch_kept(speed(1500.0, 38.0, 40.0), base));
        assert!(flash_kept(speed(1000.0, 41.5, 40.0), base));
        assert!(!flash_kept(speed(1100.0, 38.0, 40.0), base));
        assert!(!flash_kept(speed(1020.0, 40.5, 40.0), base));
        assert!(speculation_kept(speed(1000.0, 38.0, 48.0), base));
        assert!(!speculation_kept(speed(1000.0, 30.0, 55.0), base));
    }

    #[test]
    fn options_the_operator_saved_are_not_tried() {
        let options = json!({"saved": {"ubatch_size": 512, "flash_attention": null}});
        assert!(saved_flag(
            &options,
            Some("ubatch_size"),
            &[],
            &["--ubatch-size", "-ub"]
        ));
        assert!(!saved_flag(
            &options,
            Some("flash_attention"),
            &[],
            &["--flash-attn", "-fa"]
        ));
        let extra = ["-fa=off".to_owned()];
        assert!(saved_flag(
            &options,
            Some("flash_attention"),
            &extra,
            &["--flash-attn", "-fa"]
        ));
        assert!(!saved_flag(&options, None, &extra, &["--spec-type"]));
    }

    #[test]
    fn model_ids_with_a_slash_stay_one_path_segment() {
        assert_eq!(options_path("org/model"), "models/org%2Fmodel/options");
    }
}
