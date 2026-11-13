use super::{Manager, ModelView, Worker, WorkerState, publication, unix_time_ms};
use crate::{
    ApiError,
    config::{Config, MAXIMUM_IDLE_UNLOAD_MS, Policy},
};
use axum::http::StatusCode;
use serde::{Deserialize, Deserializer, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Mutex as AsyncMutex;

const SWEEP_INTERVAL: Duration = Duration::from_millis(100);
const HOLDERS_SHOWN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum UnloadReason {
    Idle,
    Pressure,
    Operator,
}

impl UnloadReason {
    fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Pressure => "pressure",
            Self::Operator => "operator",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeepAlive {
    Forever,
    After(Duration),
}

#[derive(Clone, Debug, Serialize)]
pub struct UnloadEvent {
    pub reason: UnloadReason,
    pub at_unix_ms: u64,
    pub generation: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ResidencyView {
    pub idle_unload_after_ms: Option<u64>,
    pub source: &'static str,
    pub model_idle_unload_after_ms: Option<u64>,
    pub keep_alive_ms: Option<i64>,
    pub last_used_unix_ms: Option<u64>,
    pub idle_unload_at_unix_ms: Option<u64>,
    pub last_unload: Option<UnloadEvent>,
}

#[derive(Default)]
struct Entry {
    last_used: Option<(Instant, u64)>,
    keep_alive: Option<KeepAlive>,
    last_unload: Option<UnloadEvent>,
}

#[derive(Default)]
pub(super) struct Residency {
    default_ms: AtomicU64,
    configured: Mutex<HashMap<String, Option<u64>>>,
    entries: Mutex<HashMap<String, Entry>>,
    eviction: AsyncMutex<()>,
}

struct Effective {
    after: Option<Duration>,
    source: &'static str,
}

impl Residency {
    pub(super) fn new(config: &Config) -> Self {
        let residency = Self::default();
        residency.refresh(config);
        residency
    }

    pub(super) fn refresh(&self, config: &Config) {
        self.default_ms
            .store(config.idle_unload_after_ms, Ordering::Release);
        *self
            .configured
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = config
            .models
            .iter()
            .map(|model| (model.id.clone(), model.idle_unload_after_ms))
            .collect();
    }

    pub(super) fn touch(&self, id: &str) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries.entry(id.to_owned()).or_default().last_used =
            Some((Instant::now(), unix_time_ms()));
    }

    fn set_keep_alive(&self, id: &str, keep_alive: KeepAlive) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        entries.entry(id.to_owned()).or_default().keep_alive = Some(keep_alive);
    }

    fn record_unload(&self, id: &str, reason: UnloadReason, generation: u64) {
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let entry = entries.entry(id.to_owned()).or_default();
        entry.keep_alive = None;
        entry.last_unload = Some(UnloadEvent {
            reason,
            at_unix_ms: unix_time_ms(),
            generation,
        });
    }

    fn configured(&self, id: &str) -> Option<u64> {
        self.configured
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(id)
            .copied()
            .flatten()
    }

    fn effective(&self, id: &str, keep_alive: Option<KeepAlive>) -> Effective {
        let never_when_zero = |ms: u64| (ms != 0).then(|| Duration::from_millis(ms));
        match (keep_alive, self.configured(id)) {
            (Some(KeepAlive::Forever), _) => Effective {
                after: None,
                source: "keep_alive",
            },
            (Some(KeepAlive::After(after)), _) => Effective {
                after: Some(after),
                source: "keep_alive",
            },
            (None, Some(ms)) => Effective {
                after: never_when_zero(ms),
                source: "model",
            },
            (None, None) => Effective {
                after: never_when_zero(self.default_ms.load(Ordering::Acquire)),
                source: "service",
            },
        }
    }

    fn last_used(&self, id: &str) -> Option<Instant> {
        self.entries
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(id)
            .and_then(|entry| entry.last_used.map(|(instant, _)| instant))
    }

    fn expired(&self, id: &str, now: Instant) -> bool {
        let (keep_alive, last_used) = {
            let entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let Some(entry) = entries.get(id) else {
                return false;
            };
            (entry.keep_alive, entry.last_used)
        };
        let Some((used, _)) = last_used else {
            return false;
        };
        self.effective(id, keep_alive)
            .after
            .is_some_and(|after| now.saturating_duration_since(used) >= after)
    }

    pub(super) fn view(&self, model: &ModelView) -> ResidencyView {
        let (keep_alive, last_used, last_unload) = {
            let entries = self
                .entries
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            entries.get(&model.id).map_or((None, None, None), |entry| {
                (
                    entry.keep_alive,
                    entry.last_used.map(|(_, unix)| unix),
                    entry.last_unload.clone(),
                )
            })
        };
        let effective = self.effective(&model.id, keep_alive);
        let applicable = model.policy == Policy::OnDemand
            && model.state == "ready"
            && model.active_requests == 0
            && !model.preparation_pending
            && !model.startup_waiting;
        ResidencyView {
            idle_unload_after_ms: effective.after.map(duration_ms),
            source: effective.source,
            model_idle_unload_after_ms: self.configured(&model.id),
            keep_alive_ms: keep_alive.map(|keep_alive| match keep_alive {
                KeepAlive::Forever => -1,
                KeepAlive::After(after) => duration_ms(after).min(i64::MAX as u64) as i64,
            }),
            last_used_unix_ms: last_used,
            idle_unload_at_unix_ms: applicable
                .then_some(())
                .and(effective.after)
                .zip(last_used)
                .map(|(after, used)| used.saturating_add(duration_ms(after))),
            last_unload,
        }
    }
}

fn duration_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

fn double_option<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Option<u64>>, D::Error> {
    Option::<u64>::deserialize(deserializer).map(Some)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResidencyChange {
    expected_generation: u64,
    #[serde(default)]
    model: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    idle_unload_after_ms: Option<Option<u64>>,
}

fn invalid_residency(message: &'static str) -> ApiError {
    ApiError::new(StatusCode::BAD_REQUEST, "invalid_residency", message)
}

/// Parses Ollama `keep_alive`: seconds as a number or numeric string, or a
/// Go-style duration such as `5m` or `1h30m`. Negative means keep loaded.
pub fn parse_keep_alive(value: &serde_json::Value) -> Option<KeepAlive> {
    let maximum = Duration::from_millis(MAXIMUM_IDLE_UNLOAD_MS);
    let seconds = |seconds: f64| {
        if !seconds.is_finite() {
            None
        } else if seconds < 0.0 {
            Some(KeepAlive::Forever)
        } else {
            Some(KeepAlive::After(
                Duration::try_from_secs_f64(seconds)
                    .unwrap_or(maximum)
                    .min(maximum),
            ))
        }
    };
    match value {
        serde_json::Value::Number(number) => seconds(number.as_f64()?),
        serde_json::Value::String(text) => {
            let text = text.trim();
            if text.is_empty() || text.len() > 64 {
                return None;
            }
            if let Ok(number) = text.parse::<f64>() {
                return seconds(number);
            }
            let (negative, mut rest) = match text.strip_prefix('-') {
                Some(rest) => (true, rest),
                None => (false, text.strip_prefix('+').unwrap_or(text)),
            };
            let mut total = 0f64;
            while !rest.is_empty() {
                let digits = rest
                    .find(|character: char| !(character.is_ascii_digit() || character == '.'))
                    .unwrap_or(rest.len());
                if digits == 0 {
                    return None;
                }
                let amount: f64 = rest[..digits].parse().ok()?;
                rest = &rest[digits..];
                let unit = rest
                    .find(|character: char| character.is_ascii_digit() || character == '.')
                    .unwrap_or(rest.len());
                let scale = match &rest[..unit] {
                    "ns" => 1e-9,
                    "us" | "µs" => 1e-6,
                    "ms" => 1e-3,
                    "s" => 1.0,
                    "m" => 60.0,
                    "h" => 3600.0,
                    _ => return None,
                };
                rest = &rest[unit..];
                total += amount * scale;
            }
            seconds(if negative { -total } else { total })
        }
        _ => None,
    }
}

impl Manager {
    pub fn start_residency(self: &Arc<Self>) {
        let manager = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(SWEEP_INTERVAL);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                interval.tick().await;
                let Some(manager) = manager.upgrade() else {
                    return;
                };
                if manager.shutting_down.load(Ordering::Acquire) {
                    return;
                }
                manager.idle_sweep().await;
            }
        });
    }

    async fn idle_sweep(&self) {
        let now = Instant::now();
        for worker in self.worker_snapshot() {
            let expired = {
                let Ok(state) = worker.state.try_lock() else {
                    continue;
                };
                if !Self::evictable(&worker, &state) {
                    continue;
                }
                if self.residency.last_used(&worker.spec.id).is_none() {
                    self.residency.touch(&worker.spec.id);
                }
                self.residency.expired(&worker.spec.id, now)
            };
            if expired {
                self.evict(&worker, UnloadReason::Idle).await;
            }
        }
    }

    fn evictable(worker: &Worker, state: &WorkerState) -> bool {
        state.policy == Policy::OnDemand
            && state.running.is_some()
            && worker.active.load(Ordering::Acquire) == 0
            && worker.uncertain_requests.load(Ordering::Acquire) == 0
            && worker.uncertain_state_operations.load(Ordering::Acquire) == 0
            && worker.speech_retiring.load(Ordering::Acquire) == 0
            && !worker.preparing.load(Ordering::Acquire)
            && !worker.startup_waiting.load(Ordering::Acquire)
            && !worker.slots.in_use()
            && worker
                .processes
                .snapshot(&worker.spec.id)
                .is_some_and(|process| process.phase == crate::process::Phase::Running)
    }

    // Session saves type-check through load, so eviction is a boxed future.
    fn evict<'a>(
        &'a self,
        worker: &'a Arc<Worker>,
        reason: UnloadReason,
    ) -> std::pin::Pin<Box<dyn Future<Output = bool> + Send + 'a>> {
        Box::pin(async move {
            self.flush_sessions(worker).await;
            let Ok(mut state) = worker.state.try_lock() else {
                return false;
            };
            Self::reap(&mut state);
            if !Self::evictable(worker, &state)
                || (reason == UnloadReason::Idle
                    && !self.residency.expired(&worker.spec.id, Instant::now()))
            {
                return false;
            }
            let run = state.running.as_mut().expect("evictable worker is running");
            let generation = run.child.generation();
            if let Err(error) = run.child.stop().await {
                state.error = Some("worker_cleanup_pending");
                Self::publish(worker, &state);
                self.logs.service(
                    crate::logs::Level::Error,
                    &format!(
                        "Worker {} generation {generation} could not be unloaded (reason: {}): {}",
                        worker.spec.id,
                        reason.label(),
                        error.code
                    ),
                );
                return false;
            }
            state.running = None;
            worker.speech_retiring.store(0, Ordering::Release);
            state.error = None;
            Self::publish(worker, &state);
            self.record_unload(worker, reason, generation);
            true
        })
    }

    pub(super) fn record_unload(&self, worker: &Worker, reason: UnloadReason, generation: u64) {
        self.residency
            .record_unload(&worker.spec.id, reason, generation);
        self.logs.service(
            crate::logs::Level::Info,
            &format!(
                "Worker {} generation {generation} unloaded (reason: {})",
                worker.spec.id,
                reason.label()
            ),
        );
    }

    fn pressure_victim(
        &self,
        requested: &Arc<Worker>,
        tried: &[Arc<Worker>],
    ) -> Option<Arc<Worker>> {
        let mut candidates = self
            .worker_snapshot()
            .into_iter()
            .filter(|worker| {
                !Arc::ptr_eq(worker, requested)
                    && !tried.iter().any(|seen| Arc::ptr_eq(seen, worker))
                    && worker
                        .state
                        .try_lock()
                        .is_ok_and(|state| Self::evictable(worker, &state))
            })
            .map(|worker| (self.residency.last_used(&worker.spec.id), worker))
            .collect::<Vec<_>>();
        candidates.sort_by_key(|(used, _)| *used);
        candidates.into_iter().next().map(|(_, worker)| worker)
    }

    /// Retries `attempt` after evicting least-recently-used idle On demand
    /// workers while it reports capacity pressure.
    pub(super) async fn reserve_with_eviction<T, F, Fut>(
        &self,
        requested: &Arc<Worker>,
        pressured: fn(&ApiError) -> bool,
        mut attempt: F,
    ) -> Result<T, ApiError>
    where
        F: FnMut() -> Fut,
        Fut: Future<Output = Result<T, ApiError>>,
    {
        match attempt().await {
            Err(error) if pressured(&error) => {}
            result => return result,
        }
        let _eviction = self.residency.eviction.lock().await;
        let mut tried = Vec::new();
        loop {
            let error = match attempt().await {
                Err(error) if pressured(&error) => error,
                result => return result,
            };
            let Some(victim) = self.pressure_victim(requested, &tried) else {
                return Err(self.pressure_error(requested, error));
            };
            self.evict(&victim, UnloadReason::Pressure).await;
            tried.push(victim);
        }
    }

    fn pressure_error(&self, requested: &Arc<Worker>, error: ApiError) -> ApiError {
        let holders = self
            .worker_snapshot()
            .into_iter()
            .filter(|worker| !Arc::ptr_eq(worker, requested))
            .filter_map(|worker| {
                let observation = worker
                    .observation
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .clone()?;
                let preparing = worker.preparing.load(Ordering::Acquire);
                if !observation.loaded && !preparing {
                    return None;
                }
                let label = if observation.policy == Policy::Hot {
                    "hot"
                } else if preparing {
                    "loading"
                } else if worker.active.load(Ordering::Acquire) != 0 || worker.slots.in_use() {
                    "busy"
                } else {
                    "not idle"
                };
                Some(format!("{} ({label})", worker.spec.id))
            })
            .collect::<Vec<_>>();
        if holders.is_empty() {
            return error;
        }
        let mut shown = holders
            .iter()
            .take(HOLDERS_SHOWN)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if holders.len() > HOLDERS_SHOWN {
            shown.push_str(&format!(" and {} more", holders.len() - HOLDERS_SHOWN));
        }
        let message = if error.code == "resident_budget" {
            format!(
                "The declared resident budget is full and no idle On demand model can be unloaded. Held by: {shown}. Wait for active work, change a Hot model to On demand, or increase the configured budget."
            )
        } else {
            format!("{} Held by: {shown}.", error.message)
        };
        ApiError::new(error.status, error.code, message)
    }

    pub fn set_keep_alive(&self, requested: &str, keep_alive: KeepAlive) -> Result<(), ApiError> {
        let (worker, _) = self.resolve_model(requested)?;
        self.residency.set_keep_alive(&worker.spec.id, keep_alive);
        Ok(())
    }

    pub(super) fn residency_overlay(&self, view: &mut ModelView) {
        view.residency = Some(self.residency.view(view));
    }

    pub fn residency_settings(&self) -> serde_json::Value {
        serde_json::json!({
            "idle_unload_after_ms": self.residency.default_ms.load(Ordering::Acquire),
        })
    }

    pub async fn residency_view(&self) -> serde_json::Value {
        let generation = self.config.lock().await.generation;
        let models = self
            .catalog()
            .await
            .into_iter()
            .map(|model| {
                let residency = model.residency.clone().expect("catalog residency");
                serde_json::json!({
                    "id": model.id, "policy": model.policy, "loaded": model.loaded,
                    "residency": residency,
                })
            })
            .collect::<Vec<_>>();
        serde_json::json!({
            "schema_version": 1,
            "configuration_generation": generation,
            "idle_unload_after_ms": self.residency.default_ms.load(Ordering::Acquire),
            "maximum_idle_unload_after_ms": MAXIMUM_IDLE_UNLOAD_MS,
            "models": models,
        })
    }

    pub async fn set_residency(
        self: &Arc<Self>,
        change: ResidencyChange,
    ) -> Result<serde_json::Value, ApiError> {
        let value = change
            .idle_unload_after_ms
            .ok_or_else(|| invalid_residency("Provide idle_unload_after_ms."))?;
        if value.is_some_and(|value| value > MAXIMUM_IDLE_UNLOAD_MS) {
            return Err(invalid_residency(
                "idle_unload_after_ms must be at most 30 days in milliseconds.",
            ));
        }
        let config = self.config.clone().lock_owned().await;
        if config.generation != change.expected_generation {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "revision_conflict",
                "Configuration changed. Refresh before changing idle unload settings.",
            ));
        }
        let mut next = config.clone();
        match &change.model {
            None => {
                next.idle_unload_after_ms = value.ok_or_else(|| {
                    invalid_residency("The service default idle_unload_after_ms must be a number.")
                })?;
            }
            Some(id) => {
                let entry = next
                    .models
                    .iter_mut()
                    .find(|entry| entry.id == *id)
                    .ok_or_else(|| {
                        ApiError::new(
                            StatusCode::NOT_FOUND,
                            "unknown_model",
                            "The model is not registered in this service.",
                        )
                    })?;
                entry.idle_unload_after_ms = value;
            }
        }
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "Configuration generation cannot advance.",
            )
        })?;
        let outcome = self
            .publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await?;
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Idle unload settings committed but directory durability is unconfirmed.",
            ));
        }
        Ok(self.residency_view().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn keep_alive_accepts_ollama_numbers_strings_and_durations() {
        let after = |ms| Some(KeepAlive::After(Duration::from_millis(ms)));
        assert_eq!(parse_keep_alive(&json!(0)), after(0));
        assert_eq!(parse_keep_alive(&json!(30)), after(30_000));
        assert_eq!(parse_keep_alive(&json!(1.5)), after(1_500));
        assert_eq!(parse_keep_alive(&json!(-1)), Some(KeepAlive::Forever));
        assert_eq!(parse_keep_alive(&json!("300")), after(300_000));
        assert_eq!(parse_keep_alive(&json!("5m")), after(300_000));
        assert_eq!(parse_keep_alive(&json!("1h30m")), after(5_400_000));
        assert_eq!(parse_keep_alive(&json!("250ms")), after(250));
        assert_eq!(parse_keep_alive(&json!("0s")), after(0));
        assert_eq!(parse_keep_alive(&json!("-1m")), Some(KeepAlive::Forever));
        assert_eq!(
            parse_keep_alive(&json!("10000h")),
            after(MAXIMUM_IDLE_UNLOAD_MS)
        );
        for invalid in [
            json!(""),
            json!("5x"),
            json!("m"),
            json!(true),
            json!(null),
            json!([1]),
        ] {
            assert_eq!(parse_keep_alive(&invalid), None, "{invalid}");
        }
    }

    fn model(id: &str, policy: Policy, state: &'static str) -> ModelView {
        ModelView {
            id: id.into(),
            artifact_set: None,
            runtime_package: None,
            implementation_id: None,
            policy,
            revision: 0,
            state,
            downloaded: true,
            loaded: state == "ready",
            pid: None,
            active_requests: 0,
            max_inflight: 1,
            resident_bytes: None,
            operations: Vec::new(),
            error: None,
            runtime_identity: None,
            process_lifecycle: None,
            preparation_pending: false,
            startup_waiting: false,
            cleanup_retry_available: false,
            uncertain_requests: 0,
            uncertain_state_operations: 0,
            request_limits: None,
            residency: None,
            vision: false,
        }
    }

    #[test]
    fn effective_setting_prefers_keep_alive_then_model_then_service() {
        let config: Config = serde_json::from_value(json!({
            "schema_version":2,"resident_budget_bytes":1,"startup_timeout_ms":1,"request_timeout_ms":1,
            "idle_unload_after_ms":1000,"models":[],
        }))
        .unwrap();
        let residency = Residency::new(&config);
        let view = residency.view(&model("m", Policy::OnDemand, "ready"));
        assert_eq!(
            (
                view.idle_unload_after_ms,
                view.source,
                view.idle_unload_at_unix_ms
            ),
            (Some(1000), "service", None)
        );
        residency.touch("m");
        let view = residency.view(&model("m", Policy::OnDemand, "ready"));
        assert_eq!(
            view.idle_unload_at_unix_ms,
            view.last_used_unix_ms.map(|used| used + 1000)
        );
        assert_eq!(
            residency
                .view(&model("m", Policy::Hot, "ready"))
                .idle_unload_at_unix_ms,
            None
        );
        residency
            .configured
            .lock()
            .unwrap()
            .insert("m".into(), Some(0));
        let view = residency.view(&model("m", Policy::OnDemand, "ready"));
        assert_eq!(
            (
                view.idle_unload_after_ms,
                view.source,
                view.idle_unload_at_unix_ms
            ),
            (None, "model", None)
        );
        assert!(!residency.expired("m", Instant::now() + Duration::from_secs(3600)));
        residency.set_keep_alive("m", KeepAlive::After(Duration::ZERO));
        let view = residency.view(&model("m", Policy::OnDemand, "ready"));
        assert_eq!(
            (view.idle_unload_after_ms, view.source),
            (Some(0), "keep_alive")
        );
        assert!(residency.expired("m", Instant::now()));
        residency.record_unload("m", UnloadReason::Idle, 3);
        let view = residency.view(&model("m", Policy::OnDemand, "unloaded"));
        assert_eq!((view.source, view.keep_alive_ms), ("model", None));
        assert_eq!(view.last_unload.unwrap().reason, UnloadReason::Idle);
    }
}
