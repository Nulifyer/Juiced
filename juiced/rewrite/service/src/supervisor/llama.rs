use super::*;
use crate::config::Provider;
use crate::engine_usage::Observation;
use serde_json::{Value, json};

const ENGINE_REPLY_BYTES: usize = 8 * 1024 * 1024;
const SLOT_PROTOCOL: &str = "openai.completions";

#[derive(Default)]
pub(super) struct UsageTotals {
    requests: AtomicU64,
    input: AtomicU64,
    output: AtomicU64,
    prompt: AtomicU64,
    cache: AtomicU64,
}

impl UsageTotals {
    pub(super) fn observe(&self, usage: &Observation) {
        let add = |counter: &AtomicU64, value: Option<u64>| {
            counter.fetch_add(value.unwrap_or(0), Ordering::Relaxed);
        };
        add(&self.requests, Some(1));
        add(
            &self.input,
            usage.prompt_processed_tokens.or(usage.input_tokens),
        );
        add(&self.output, usage.output_tokens);
        add(&self.prompt, usage.input_tokens);
        add(&self.cache, usage.cached_tokens);
    }
}

pub struct Target {
    worker: Arc<Worker>,
    base: String,
    fingerprint: Arc<str>,
}

impl Target {
    pub fn model(&self) -> &str {
        &self.worker.spec.id
    }
}

fn not_loaded() -> ApiError {
    ApiError::new(
        StatusCode::BAD_REQUEST,
        "model_not_loaded",
        "Load a llama.cpp model first, or name a loaded one with model.",
    )
}

fn engine_error(error: reqwest::Error) -> ApiError {
    if error.is_timeout() {
        ApiError::new(
            StatusCode::GATEWAY_TIMEOUT,
            "upstream_timeout",
            "The engine did not answer in time.",
        )
    } else {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "upstream_unavailable",
            "The engine request failed. Retry or inspect the selected worker.",
        )
    }
}

fn session_digest(session: &str) -> String {
    format!("{:x}", Sha256::digest(session.as_bytes()))
}

impl Manager {
    /// llama.cpp-style clients name no model for slots and tokenize, so an unnamed request
    /// goes to the loaded llama.cpp worker that served the latest request.
    pub async fn llama_target(&self, model: Option<&str>) -> Result<Target, ApiError> {
        if let Some(id) = model {
            let (worker, _) = self.resolve_model(id)?;
            if worker.spec.execution_profile.adapter != Provider::LlamaCpp {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "unsupported_operation",
                    "Only llama.cpp models expose slots and tokenization.",
                ));
            }
            let mut state = worker.state.lock().await;
            return Self::running_target(&worker, &mut state).ok_or_else(not_loaded);
        }
        let mut running = Vec::new();
        for worker in self.worker_snapshot() {
            if worker.spec.execution_profile.adapter != Provider::LlamaCpp {
                continue;
            }
            let Ok(mut state) = worker.state.try_lock() else {
                continue;
            };
            if let Some(target) = Self::running_target(&worker, &mut state) {
                running.push(target);
            }
        }
        if running.len() > 1 {
            let records = self.records();
            if let Some(index) = records.iter().find_map(|record| {
                running
                    .iter()
                    .position(|target| target.worker.record_model == record.model)
            }) {
                return Ok(running.swap_remove(index));
            }
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "model_required",
                "Several llama.cpp models are loaded. Name one with model.",
            ));
        }
        running.pop().ok_or_else(not_loaded)
    }

    fn running_target(worker: &Arc<Worker>, state: &mut WorkerState) -> Option<Target> {
        Self::reap(state);
        let run = state.running.as_ref()?;
        Some(Target {
            worker: worker.clone(),
            base: run.base.clone(),
            fingerprint: run.fingerprint.clone(),
        })
    }

    pub async fn llama_exchange(
        &self,
        target: &Target,
        method: reqwest::Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<(StatusCode, Value), ApiError> {
        let mut request = self
            .client
            .request(method, format!("{}{path}", target.base))
            .timeout(self.request_timeout);
        if let Some(body) = body {
            request = request.json(body);
        }
        let mut response = request.send().await.map_err(engine_error)?;
        let status =
            StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(engine_error)? {
            if bytes.len().saturating_add(chunk.len()) > ENGINE_REPLY_BYTES {
                return Err(ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "engine_response_limit",
                    "The engine reply exceeded its bounded size.",
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let value = serde_json::from_slice(&bytes).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "engine_response_invalid",
                "The engine reply was not JSON.",
            )
        })?;
        Ok((status, value))
    }

    /// Persistent session snapshots and explicit slot control would both own slot contents,
    /// and explicit saves are qualified only for one-slot workers.
    fn manual_slot(&self, target: &Target, slot: u32) -> Result<(), ApiError> {
        self.refuse_session_owned(&target.worker)?;
        if slot >= target.worker.spec.max_inflight.max(1) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_slot",
                "This model has no slot with that ID.",
            ));
        }
        Ok(())
    }

    pub async fn slot_erase(&self, target: &Target, slot: u32) -> Result<Value, ApiError> {
        self.manual_slot(target, slot)?;
        let (mode, _) = self.session_mode(&target.worker);
        if mode != crate::sessions::Mode::Off && target.worker.spec.max_inflight > 1 {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "cache_sessions_managed",
                "Session routing owns this model's slots. Set its session cache mode to off to erase slots by hand.",
            ));
        }
        let _slot = target
            .worker
            .slots
            .try_acquire(crate::admission::Priority::Interactive)
            .map_err(|_| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "worker_busy",
                    "Wait for active requests before erasing a slot.",
                )
            })?;
        {
            let state = target.worker.state.lock().await;
            if state
                .running
                .as_ref()
                .is_none_or(|run| run.fingerprint != target.fingerprint)
            {
                return Err(not_loaded());
            }
            *target
                .worker
                .cache_identity
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = None;
        }
        let (status, reply) = self
            .llama_exchange(
                target,
                reqwest::Method::POST,
                &format!("/slots/{slot}?action=erase"),
                Some(&json!({})),
            )
            .await?;
        if !status.is_success() {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "slot_erase_rejected",
                "The engine rejected the slot erase.",
            ));
        }
        Ok(json!({"id_slot": slot, "n_erased": reply["n_erased"]}))
    }

    pub async fn slot_save(
        &self,
        target: &Target,
        slot: u32,
        filename: String,
        maximum_bytes: Option<u64>,
        context: RequestContext,
    ) -> Result<CacheReply<Value>, ApiError> {
        self.manual_slot(target, slot)?;
        let cache = self.cache_store()?;
        let replaced = self.slot_snapshots(&cache, &context, &filename).await?;
        let request = CacheSave {
            model: target.model().to_owned(),
            slot,
            session: filename.clone(),
            expected_generation: cache.generation(),
            maximum_bytes: maximum_bytes
                .unwrap_or_else(|| self.session_settings().model_budget_bytes),
        };
        let reply = self.save_snapshot(request, context.clone()).await?;
        let mut kept = None;
        let session = filename.clone();
        let reply = reply.map(|value| {
            kept = value["snapshot"]["id"].as_str().map(str::to_owned);
            json!({
                "id_slot": slot, "filename": filename,
                "n_saved": value["engine"]["n_saved"], "n_written": value["engine"]["n_written"],
                "juiced": {"snapshot": value["snapshot"]["id"], "bytes": value["snapshot"]["bytes"]},
            })
        });
        // llama.cpp overwrites a filename on save, so earlier snapshots of it are retired.
        tokio::task::spawn_blocking(move || {
            for old in replaced {
                if Some(&old.id) == kept.as_ref() {
                    continue;
                }
                let identity = snapshot_identity(
                    &old.execution_fingerprint,
                    &context,
                    &session,
                    &old.protocol,
                );
                let _ = cache.forget(&old.id, &identity, old.revision);
            }
        });
        Ok(reply)
    }

    pub async fn slot_restore(
        &self,
        target: &Target,
        slot: u32,
        filename: String,
        context: RequestContext,
    ) -> Result<CacheReply<Value>, ApiError> {
        self.manual_slot(target, slot)?;
        let cache = self.cache_store()?;
        let mut saved = self.slot_snapshots(&cache, &context, &filename).await?;
        saved.sort_by_key(|snapshot| snapshot.execution_fingerprint != *target.fingerprint);
        let snapshot = saved.into_iter().next().ok_or_else(|| {
            ApiError::new(
                StatusCode::NOT_FOUND,
                "slot_file_not_found",
                "No saved slot uses this filename for this credential.",
            )
        })?;
        let request = CacheRestore {
            model: target.model().to_owned(),
            slot,
            session: filename.clone(),
            snapshot: snapshot.id,
        };
        let reply = self.restore_snapshot(request, context).await?;
        Ok(reply.map(|value| {
            json!({
                "id_slot": slot, "filename": filename,
                "n_restored": value["engine"]["n_restored"], "n_read": value["engine"]["n_read"],
                "juiced": {"snapshot": value["snapshot"]["id"]},
            })
        }))
    }

    async fn slot_snapshots(
        &self,
        cache: &Arc<CacheStore>,
        context: &RequestContext,
        filename: &str,
    ) -> Result<Vec<crate::cache::SnapshotMetadata>, ApiError> {
        Self::cache_session(filename)?;
        let cache = cache.clone();
        let principal = context.principal;
        let epoch = context.credential_epoch.clone();
        let digest = session_digest(filename);
        tokio::task::spawn_blocking(move || {
            cache.list(principal, &epoch).map(|rows| {
                rows.into_iter()
                    .filter(|row| {
                        row.state == crate::cache::SnapshotState::Saved
                            && row.protocol == SLOT_PROTOCOL
                            && row.session_digest == digest
                    })
                    .collect()
            })
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache_observation_interrupted",
                "The snapshot inventory observation was interrupted.",
            )
        })?
        .map_err(cache_error)
    }

    pub fn llama_stats(&self) -> Value {
        let last = self
            .last_inference
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let usage = last.as_ref().map(|last| last.usage).unwrap_or_default();
        let first_token = usage
            .prompt_processed_tokens
            .zip(usage.prompt_tokens_per_second)
            .filter(|(_, rate)| *rate > 0.0)
            .map(|(tokens, rate)| tokens as f64 / rate);
        let totals = &self.usage_totals;
        let total = |counter: &AtomicU64| counter.load(Ordering::Relaxed);
        json!({
            "time_to_first_token": first_token.unwrap_or(0.0),
            "tokens_per_second": usage.decode_tokens_per_second.unwrap_or(0.0),
            "input_tokens": usage.prompt_processed_tokens.or(usage.input_tokens).unwrap_or(0),
            "output_tokens": usage.output_tokens.unwrap_or(0),
            "prompt_tokens": usage.input_tokens.unwrap_or(0),
            "cache_tokens": usage.cached_tokens,
            "request_count_total": total(&totals.requests),
            "input_tokens_total": total(&totals.input),
            "output_tokens_total": total(&totals.output),
            "prompt_tokens_total": total(&totals.prompt),
            "cache_tokens_total": total(&totals.cache),
            "routing_decisions_total": 0,
            "routing_switches_total": 0,
            "live": {
                "active_requests": self.active_requests(),
                "active_scope": "admitted-requests",
                "generating_requests": null,
                "estimated_tokens_per_second": null,
            },
            "last_request": last.map(|last| json!({
                "model": last.model, "operation": last.operation,
                "completed_at_ms": last.completed_at_ms, "elapsed_ms": last.elapsed_ms,
            })),
        })
    }

    pub async fn audio_voices(&self, model: &str) -> Result<Value, ApiError> {
        let (worker, _) = self.resolve_model(model)?;
        if worker.spec.execution_profile.adapter != Provider::Kokoros {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "voices_unsupported",
                "This backend does not expose voice discovery.",
            ));
        }
        let listed = self
            .voices(Some(&worker.spec.id))
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();
        if !listed["error"].is_null() {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "voice_discovery_failed",
                "The speech model's voice pack could not be read.",
            ));
        }
        let mut voices: Vec<&str> = listed["voices"]
            .as_array()
            .map(|voices| voices.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        voices.sort_unstable();
        voices.dedup();
        Ok(json!({"model": model, "voices": voices, "source": "voice-pack"}))
    }
}
