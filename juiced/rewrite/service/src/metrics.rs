use crate::{
    auth::{Authority, Authorization},
    config::Operation,
    engine_usage::Observation,
    supervisor::Manager,
};
use axum::{
    Router,
    extract::{
        State,
        ws::{WebSocketUpgrade, rejection::WebSocketUpgradeRejection},
    },
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt::Write,
    sync::{Arc, Mutex},
};

pub const CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

#[derive(Clone, Copy, Default)]
struct Totals {
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    prompt_tokens: u64,
    cache_tokens: u64,
}

impl Totals {
    fn add(&mut self, usage: &Observation) {
        self.requests += 1;
        self.input_tokens += usage.input_tokens.unwrap_or(0);
        self.output_tokens += usage.output_tokens.unwrap_or(0);
        self.prompt_tokens += usage.prompt_processed_tokens.unwrap_or(0);
        self.cache_tokens += usage.cached_tokens.unwrap_or(0);
    }
}

#[derive(Default)]
struct ModelCounters {
    totals: Totals,
    latest: Observation,
}

/// Process-lifetime inference counters, as the native server kept them.
#[derive(Default)]
pub struct Counters {
    inner: Mutex<(Totals, BTreeMap<Arc<str>, ModelCounters>)>,
}

impl Counters {
    pub fn observe(&self, model: &Arc<str>, usage: &Observation) {
        let mut inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        inner.0.add(usage);
        let entry = inner.1.entry(model.clone()).or_default();
        entry.totals.add(usage);
        if !usage.is_empty() {
            entry.latest = *usage;
        }
    }

    fn snapshot(&self) -> (Totals, BTreeMap<Arc<str>, (Totals, Observation)>) {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        (
            inner.0,
            inner
                .1
                .iter()
                .map(|(model, counters)| (model.clone(), (counters.totals, counters.latest)))
                .collect(),
        )
    }
}

struct Exposition {
    text: String,
}

impl Exposition {
    fn family(&mut self, name: &str, kind: &str, help: &str) {
        let _ = writeln!(self.text, "# HELP {name} {help}\n# TYPE {name} {kind}");
    }

    fn sample(&mut self, name: &str, labels: &[(&str, &str)], value: f64) {
        self.text.push_str(name);
        if !labels.is_empty() {
            self.text.push('{');
            for (index, (key, value)) in labels.iter().enumerate() {
                if index > 0 {
                    self.text.push(',');
                }
                let escaped = value
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
                    .replace('\n', "\\n");
                let _ = write!(self.text, "{key}=\"{escaped}\"");
            }
            self.text.push('}');
        }
        let _ = writeln!(self.text, " {value}");
    }
}

fn model_type(operations: &[Operation]) -> &'static str {
    let names: BTreeSet<&str> = operations
        .iter()
        .map(|operation| operation.path())
        .collect();
    if names.contains("chat/completions") || names.contains("completions") {
        "llm"
    } else if names.contains("embeddings") {
        "embedding"
    } else if names.contains("audio/transcriptions") {
        "transcription"
    } else if names.contains("audio/speech") {
        "tts"
    } else if names.iter().any(|name| name.starts_with("images/")) {
        "image"
    } else {
        ""
    }
}

type Gauge = (
    &'static str,
    &'static str,
    &'static str,
    fn(&Observation) -> Option<f64>,
);
type Counter = (&'static str, &'static str, &'static str, fn(&Totals) -> u64);

pub async fn render(manager: &Arc<Manager>) -> String {
    let catalog = manager.catalog().await;
    let stats = manager.system_stats().await;
    let (totals, models) = manager.metrics().snapshot();
    let mut out = Exposition {
        text: String::with_capacity(4096),
    };
    out.family(
        "lemonade_server_up",
        "gauge",
        "Whether the Lemonade server is running.",
    );
    out.sample("lemonade_server_up", &[], 1.0);
    out.family(
        "lemonade_server_info",
        "gauge",
        "Lemonade server build information.",
    );
    out.sample("lemonade_server_info", &[("version", crate::VERSION)], 1.0);
    out.family(
        "lemonade_loaded_models",
        "gauge",
        "Number of models currently loaded in Lemonade.",
    );
    out.sample(
        "lemonade_loaded_models",
        &[],
        catalog.iter().filter(|model| model.loaded).count() as f64,
    );
    let observed: Vec<_> = catalog
        .iter()
        .filter(|model| model.loaded || models.contains_key(model.id.as_str()))
        .map(|model| {
            let labels = [
                ("model_name", model.id.as_str()),
                ("checkpoint", ""),
                ("type", model_type(&model.operations)),
                ("device", ""),
                ("recipe", model.implementation_id.unwrap_or_default()),
            ];
            (labels, model.loaded, models.get(model.id.as_str()).copied())
        })
        .collect();
    let gauges: [Gauge; 7] = [
        (
            "lemonade_model_info",
            "gauge",
            "Metadata for each Lemonade model observed by this process.",
            |_| Some(1.0),
        ),
        (
            "lemonade_model_input_tokens",
            "gauge",
            "Latest input token count reported by a model.",
            |usage| usage.input_tokens.map(|value| value as f64),
        ),
        (
            "lemonade_model_output_tokens",
            "gauge",
            "Latest output token count reported by a model.",
            |usage| usage.output_tokens.map(|value| value as f64),
        ),
        (
            "lemonade_model_prompt_tokens",
            "gauge",
            "Latest prompt token count reported by a model.",
            |usage| usage.prompt_processed_tokens.map(|value| value as f64),
        ),
        (
            "lemonade_model_cache_tokens",
            "gauge",
            "Latest prompt tokens served from the backend prefix cache for a model.",
            |usage| usage.cached_tokens.map(|value| value as f64),
        ),
        (
            "lemonade_model_tokens_per_second",
            "gauge",
            "Latest generation throughput reported by a model.",
            |usage| usage.decode_tokens_per_second,
        ),
        (
            "lemonade_model_prompt_tokens_per_second",
            "gauge",
            "Latest prompt processing throughput reported by a model.",
            |usage| usage.prompt_tokens_per_second,
        ),
    ];
    for (name, kind, help, value) in gauges {
        out.family(name, kind, help);
        for (labels, _, counters) in &observed {
            let usage = counters.map(|(_, latest)| latest).unwrap_or_default();
            if let Some(value) = value(&usage) {
                out.sample(name, labels, value);
            }
        }
        if name == "lemonade_model_info" {
            out.family(
                "lemonade_model_loaded",
                "gauge",
                "Whether this model is currently loaded in Lemonade.",
            );
            for (labels, loaded, _) in &observed {
                out.sample(
                    "lemonade_model_loaded",
                    labels,
                    f64::from(u8::from(*loaded)),
                );
            }
        }
    }
    let counters: [Counter; 5] = [
        (
            "lemonade_model_requests_total",
            "lemonade_requests_total",
            "inference requests",
            |totals| totals.requests,
        ),
        (
            "lemonade_model_input_tokens_total",
            "lemonade_input_tokens_total",
            "input tokens",
            |totals| totals.input_tokens,
        ),
        (
            "lemonade_model_output_tokens_total",
            "lemonade_output_tokens_total",
            "output tokens",
            |totals| totals.output_tokens,
        ),
        (
            "lemonade_model_prompt_tokens_total",
            "lemonade_prompt_tokens_total",
            "prompt tokens",
            |totals| totals.prompt_tokens,
        ),
        (
            "lemonade_model_cache_tokens_total",
            "lemonade_cache_tokens_total",
            "prompt tokens served from the backend prefix cache",
            |totals| totals.cache_tokens,
        ),
    ];
    for (per_model, _, what, value) in counters {
        out.family(
            per_model,
            "counter",
            &format!("Cumulative {what} observed for a model."),
        );
        for (labels, _, counters) in &observed {
            let totals = counters.map(|(totals, _)| totals).unwrap_or_default();
            out.sample(per_model, labels, value(&totals) as f64);
        }
    }
    for (_, overall, what, value) in counters {
        out.family(
            overall,
            "counter",
            &format!("Cumulative {what} observed by Lemonade."),
        );
        out.sample(overall, &[], value(&totals) as f64);
    }
    let system = [
        (
            "lemonade_cpu_usage_percent",
            "cpu_percent",
            "System CPU utilization percentage.",
        ),
        (
            "lemonade_memory_used_gb",
            "memory_gb",
            "System memory usage in GiB.",
        ),
        (
            "lemonade_gpu_usage_percent",
            "gpu_percent",
            "Primary GPU utilization percentage.",
        ),
        (
            "lemonade_vram_used_gb",
            "vram_gb",
            "Primary GPU memory usage in GiB.",
        ),
        (
            "lemonade_npu_usage_percent",
            "npu_percent",
            "NPU utilization percentage.",
        ),
    ];
    for (name, field, help) in system {
        out.family(name, "gauge", help);
        if let Some(value) = stats[field]
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
        {
            out.sample(name, &[], value);
        }
    }
    out.text
}

async fn metrics(State(manager): State<Arc<Manager>>) -> Response {
    (
        [
            (header::CONTENT_TYPE, CONTENT_TYPE),
            (header::CACHE_CONTROL, "no-store"),
        ],
        render(&manager).await,
    )
        .into_response()
}

pub async fn log_stream(
    manager: Arc<Manager>,
    auth: Arc<Authorization>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    let Ok(upgrade) = upgrade else {
        return crate::ApiError::new(
            StatusCode::BAD_REQUEST,
            "websocket_required",
            "Open this endpoint as a WebSocket and send logs.subscribe.",
        )
        .into_response();
    };
    let buffer = manager.logs().clone();
    upgrade.on_upgrade(move |socket| {
        crate::logs::stream(buffer, socket, move |token| {
            let mut headers = HeaderMap::new();
            format!("Bearer {token}").parse().is_ok_and(|value| {
                headers.insert(header::AUTHORIZATION, value);
                auth.authenticate(&headers, Authority::Read).is_ok()
            })
        })
    })
}

/// Unprefixed operational routes: `GET /metrics` and the bare `/logs/stream` WebSocket.
pub fn router(manager: Arc<Manager>, auth: Arc<Authorization>) -> Router {
    Router::new()
        .route("/metrics", get(metrics))
        .route(
            "/logs/stream",
            get(
                move |State(manager): State<Arc<Manager>>,
                      upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>| {
                    log_stream(manager, auth.clone(), upgrade)
                },
            ),
        )
        .with_state(manager)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_per_model_and_keep_the_latest_observation() {
        let counters = Counters::default();
        let model: Arc<str> = Arc::from("m");
        counters.observe(
            &model,
            &Observation {
                input_tokens: Some(10),
                output_tokens: Some(4),
                cached_tokens: Some(6),
                prompt_processed_tokens: Some(4),
                decode_tokens_per_second: Some(20.0),
                prompt_tokens_per_second: None,
            },
        );
        counters.observe(&model, &Observation::default());
        let (totals, models) = counters.snapshot();
        assert_eq!(
            (totals.requests, totals.input_tokens, totals.cache_tokens),
            (2, 10, 6)
        );
        let (per_model, latest) = models["m"];
        assert_eq!(per_model.output_tokens, 4);
        assert_eq!(latest.decode_tokens_per_second, Some(20.0));
    }

    #[test]
    fn label_values_are_escaped() {
        let mut out = Exposition {
            text: String::new(),
        };
        out.sample("x", &[("a", "q\"\\\n")], 1.5);
        assert_eq!(out.text, "x{a=\"q\\\"\\\\\\n\"} 1.5\n");
    }
}
