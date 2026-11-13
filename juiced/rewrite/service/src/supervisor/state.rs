use super::*;
use crate::cache::{BoundSnapshotRead, OperationControl, PendingSnapshot, WriterRequest};
use tokio::sync::OwnedMutexGuard;

#[derive(Default)]
pub(super) struct RetainedStates(Mutex<Option<RetainedState>>);

struct StateClaims {
    physical: Option<PhysicalClaims>,
    uncertain: bool,
}

impl Drop for StateClaims {
    fn drop(&mut self) {
        if self.uncertain
            && let Some(worker) = self
                .physical
                .as_ref()
                .and_then(|physical| physical.worker.upgrade())
        {
            worker
                .uncertain_state_operations
                .fetch_sub(1, Ordering::AcqRel);
        }
    }
}

enum FileOwner {
    Pending(PendingSnapshot),
    Reader(BoundSnapshotRead),
}

struct RetainedState {
    _file: Option<FileOwner>,
    _claims: StateClaims,
}

#[derive(Default)]
pub(super) struct Operations {
    active: AtomicU32,
    policy_bound: AtomicU32,
    settled: tokio::sync::Notify,
}

impl Operations {
    pub(super) fn active(&self) -> u32 {
        self.active.load(Ordering::Acquire)
    }

    pub(super) fn policy_bound(&self) -> u32 {
        self.policy_bound.load(Ordering::Acquire)
    }

    pub(super) fn enter(self: &Arc<Self>, policy_bound: bool) -> Arc<CacheActivity> {
        self.active.fetch_add(1, Ordering::AcqRel);
        if policy_bound {
            self.policy_bound.fetch_add(1, Ordering::AcqRel);
        }
        Arc::new(CacheActivity {
            owner: self.clone(),
            policy_bound,
            _memory: None,
        })
    }

    pub(super) async fn drain(&self, deadline: tokio::time::Instant) -> bool {
        loop {
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            if self.active() == 0 {
                return true;
            }
            if tokio::time::timeout_at(deadline, settled).await.is_err() {
                return false;
            }
        }
    }
}

pub(super) struct CacheActivity {
    owner: Arc<Operations>,
    policy_bound: bool,
    _memory: Option<crate::cache::memory::Scratch>,
}

impl CacheActivity {
    pub(super) fn response_limit(&self) -> u64 {
        self._memory
            .as_ref()
            .map(|memory| memory.response_limit())
            .unwrap_or_else(|| {
                crate::cache::memory::Bounds::for_limits(&crate::cache::CacheLimits::default())
                    .expect("default cache limits are valid")
                    .response_bytes
            })
    }
}

impl Drop for CacheActivity {
    fn drop(&mut self) {
        if self.policy_bound {
            self.owner.policy_bound.fetch_sub(1, Ordering::AcqRel);
        }
        if self.owner.active.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.owner.settled.notify_waiters();
        }
    }
}

pub(super) struct Cancellation(Arc<AtomicBool>);

pub(super) type ClaimCell = Arc<Mutex<Option<PhysicalClaims>>>;

enum PhysicalSlot {
    Lent,
    Owned(crate::admission::Slot),
}

impl Drop for Cancellation {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

pub(super) struct StateOperation {
    worker: Arc<Worker>,
    file: Option<FileOwner>,
    claims: Option<StateClaims>,
    generation: Arc<RetainedStates>,
    exchange_pending: bool,
    control: OperationControl,
    _activity: Arc<CacheActivity>,
    lender: Option<ClaimCell>,
    guard: OwnedMutexGuard<WorkerState>,
}

impl StateOperation {
    pub(super) fn generation(&self) -> u64 {
        self.run().child.generation()
    }

    pub(super) fn fingerprint(&self) -> Arc<str> {
        self.run().fingerprint.clone()
    }

    fn capture_actor(&mut self) -> Result<WriterRequest, ApiError> {
        let run = self.guard.running.as_mut().expect("owned generation");
        let pid = run.child.id().ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "worker_exited",
                "The state writer exited before dispatch.",
            )
        })?;
        let generation = run.child.generation();
        let actor = crate::writer_identity::capture_owned(
            pid,
            generation,
            self.control.deadline(),
            || self.control.cancelled().load(Ordering::Acquire),
            |pid, generation| {
                run.child.generation() == generation
                    && run.child.id() == Some(pid)
                    && matches!(run.child.try_wait(), Ok(None))
            },
        );
        let actor = match actor {
            Ok(actor) => Some(actor),
            Err(
                crate::writer_identity::Reason::UnsupportedPlatform
                | crate::writer_identity::Reason::UnsupportedPidfs,
            ) => None,
            Err(crate::writer_identity::Reason::Deadline) => return Err(state_deadline_error()),
            Err(crate::writer_identity::Reason::Cancelled) => {
                return Err(cache_error(CacheError::Cancelled));
            }
            Err(_) => {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "cache_writer_unavailable",
                    "The exact state writer could not be captured before its durable intent.",
                ));
            }
        };
        self.check()?;
        Ok(WriterRequest {
            model: self.worker.spec.id.clone(),
            generation,
            actor,
        })
    }

    fn validate_actor(&mut self, writer: &WriterRequest) -> Result<(), ApiError> {
        self.check()?;
        let run = self.guard.running.as_mut().expect("owned generation");
        if run.child.generation() != writer.generation || !matches!(run.child.try_wait(), Ok(None))
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "cache_writer_changed",
                "The owned state process exited before dispatch.",
            ));
        }
        if let Some(actor) = &writer.actor {
            let observation =
                crate::writer_identity::observe(actor, self.control.deadline(), || {
                    self.control.cancelled().load(Ordering::Acquire)
                });
            if observation.state != crate::writer_identity::State::Running {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "cache_writer_changed",
                    "The bound state process could not be revalidated before dispatch.",
                ));
            }
        }
        self.check()
    }

    fn run(&self) -> &Running {
        self.guard
            .running
            .as_ref()
            .expect("state operation owns its running generation")
    }

    fn check(&self) -> Result<(), ApiError> {
        self.control.check().map_err(cache_error)
    }

    async fn blocking<T: Send + 'static>(
        self,
        work: impl FnOnce(&mut Self) -> Result<T, ApiError> + Send + 'static,
    ) -> Result<(Self, T), ApiError> {
        tokio::task::spawn_blocking(move || {
            let mut owner = self;
            owner.check()?;
            let result = work(&mut owner);
            owner.check()?;
            Ok((owner, result?))
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "state_operation_interrupted",
                "Snapshot work was interrupted. Retained ownership must settle before reuse.",
            )
        })?
    }

    async fn verify(self) -> Result<Self, ApiError> {
        self.blocking(|owner| {
            let run = owner.run();
            let observation = run.runtime_identity.as_ref().map_err(|error| {
                ApiError::new(StatusCode::NOT_IMPLEMENTED, error.code, error.message)
            })?;
            let deadline = owner.control.deadline();
            let result = run
                .child
                .resources()
                .plan
                .verify_artifact_fresh(deadline)
                .and_then(|()| {
                    if let Some(bundle) = run.child.resources().plan.bundle() {
                        bundle.verify_fresh(deadline)?;
                    }
                    crate::runtime_identity::verify_generation(observation, deadline)
                });
            let mut summary = run
                .runtime_summary
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            match &result {
                Ok(()) => {
                    summary.state = "observed";
                    summary.reason = None;
                    summary.verified_at_unix_ms = Some(unix_time_ms());
                }
                Err(error) => {
                    summary.state = "stale";
                    summary.reason = Some(error.code);
                }
            }
            result.map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))
        })
        .await
        .map(|(owner, ())| owner)
    }

    async fn exchange(
        &mut self,
        client: &reqwest::Client,
        request: reqwest::RequestBuilder,
    ) -> Result<(StatusCode, Vec<u8>), ApiError> {
        self.check()?;
        let mut request = request.build().map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "state_request_invalid",
                "The state request could not be prepared before dispatch.",
            )
        })?;
        *request.timeout_mut() = Some(
            self.control
                .deadline()
                .saturating_duration_since(Instant::now()),
        );
        self.exchange_pending = true;
        let deadline = tokio::time::Instant::from_std(self.control.deadline());
        let read = async {
            let mut response = client
                .execute(request)
                .await
                .map_err(|_| state_transport_error())?;
            let status = response.status();
            let mut bytes = Vec::with_capacity(1024);
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| state_transport_error())?
            {
                if chunk.len() > 1048576usize.saturating_sub(bytes.len()) {
                    return Err(state_transport_error());
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok((status, bytes))
        };
        let result = tokio::time::timeout_at(deadline, read)
            .await
            .map_err(|_| state_deadline_error())??;
        // Only the original response EOF releases transport uncertainty, even for an error status.
        self.exchange_pending = false;
        self.check()?;
        Ok(result)
    }

    async fn inspect(
        &mut self,
        client: &reqwest::Client,
    ) -> Result<
        (
            crate::profiles::PropsObservation,
            Vec<crate::profiles::SlotObservation>,
        ),
        ApiError,
    > {
        let base = self.run().base.clone();
        let (status, props) = self
            .exchange(client, client.get(format!("{base}/props")))
            .await?;
        if !status.is_success() {
            return Err(state_observation_error());
        }
        let props = crate::profiles::parse_props(&props).map_err(|_| state_observation_error())?;
        let (status, slots) = self
            .exchange(client, client.get(format!("{base}/slots")))
            .await?;
        if !status.is_success() {
            return Err(state_observation_error());
        }
        let slots = crate::profiles::parse_slots(&slots).map_err(|_| state_observation_error())?;
        props
            .validate_capacity(&slots, self.worker.spec.max_inflight)
            .map_err(|_| state_observation_error())?;
        Ok((props, slots))
    }
}

impl Drop for StateOperation {
    fn drop(&mut self) {
        if !self.exchange_pending {
            if let Some(cell) = &self.lender
                && let Some(physical) = self
                    .claims
                    .as_mut()
                    .and_then(|claims| claims.physical.take())
            {
                *cell.lock().unwrap_or_else(|error| error.into_inner()) = Some(physical);
            }
            return;
        }
        if let Some(mut claims) = self.claims.take() {
            claims.uncertain = true;
            self.worker
                .uncertain_state_operations
                .fetch_add(1, Ordering::AcqRel);
            let retained = RetainedState {
                _claims: claims,
                _file: self.file.take(),
            };
            let mut generation = self
                .generation
                .0
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if generation.is_none() {
                *generation = Some(retained);
            } else {
                self.worker
                    .physical_invariant_failed
                    .store(true, Ordering::Release);
                std::mem::forget(retained);
            }
        }
    }
}

fn state_transport_error() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "state_transport_uncertain",
        "The original state response did not reach completion. Its generation retains physical and file ownership until acknowledged cleanup.",
    )
}

fn state_deadline_error() -> ApiError {
    ApiError::new(
        StatusCode::GATEWAY_TIMEOUT,
        "cache_deadline",
        "The snapshot operation reached its completion deadline. Inspect retained ownership before retrying.",
    )
}

fn state_observation_error() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "state_observation_invalid",
        "The engine did not report the selected physical slot capacity.",
    )
}

fn acknowledgement(
    bytes: &[u8],
    filename: &str,
    slot: u32,
    tokens: &str,
    count: &str,
) -> Result<serde_json::Value, ApiError> {
    let invalid = || {
        ApiError::new(
            StatusCode::BAD_GATEWAY,
            "cache_acknowledgement_uncertain",
            "The engine did not confirm the requested slot operation. Inspect retained state or rebuild the context cold.",
        )
    };
    #[derive(serde::Deserialize)]
    struct Acknowledgement<'a> {
        #[serde(borrow)]
        id_slot: Option<&'a serde_json::value::RawValue>,
        #[serde(borrow)]
        filename: Option<&'a serde_json::value::RawValue>,
        #[serde(borrow)]
        n_saved: Option<&'a serde_json::value::RawValue>,
        #[serde(borrow)]
        n_written: Option<&'a serde_json::value::RawValue>,
        #[serde(borrow)]
        n_restored: Option<&'a serde_json::value::RawValue>,
        #[serde(borrow)]
        n_read: Option<&'a serde_json::value::RawValue>,
    }
    let value: Acknowledgement<'_> = serde_json::from_slice(bytes).map_err(|_| invalid())?;
    let number = |raw: Option<&serde_json::value::RawValue>| {
        raw.filter(|raw| raw.get().len() <= 20)
            .and_then(|raw| serde_json::from_str::<u64>(raw.get()).ok())
    };
    let observed_filename = value
        .filename
        .filter(|raw| raw.get().len() <= filename.len().saturating_mul(6).saturating_add(2))
        .and_then(|raw| serde_json::from_str::<String>(raw.get()).ok());
    let (observed_tokens, observed_count) = match (tokens, count) {
        ("n_saved", "n_written") => (number(value.n_saved), number(value.n_written)),
        ("n_restored", "n_read") => (number(value.n_restored), number(value.n_read)),
        _ => return Err(invalid()),
    };
    if number(value.id_slot) != Some(u64::from(slot))
        || observed_filename.as_deref() != Some(filename)
        || !observed_tokens.is_some_and(|value| value > 0)
        || !observed_count.is_some_and(|value| value > 0)
    {
        return Err(invalid());
    }
    Ok(serde_json::json!({"id_slot":slot,(tokens):observed_tokens,(count):observed_count}))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StateAction {
    Inspect,
    Save,
    Restore,
}

pub(super) struct StateBudget {
    bytes: Option<u64>,
    priority: crate::admission::Priority,
    deadline: Instant,
}

impl Manager {
    pub(super) async fn cache_activity(
        &self,
        policy_bound: bool,
        work: crate::cache::memory::Work,
    ) -> Result<Arc<CacheActivity>, ApiError> {
        let closed = || {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "shutting_down",
                "The service is shutting down.",
            )
        };
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(closed());
        }
        let mut activity = self.cache_operations.enter(policy_bound);
        if self.memory_protected() {
            let memory = self.cache_store()?.control_memory().ok_or_else(closed)?;
            self.host_memory.refresh(&self.admission).await?;
            Arc::get_mut(&mut activity)
                .expect("new cache activity has one owner")
                ._memory = Some(memory.reserve(work).map_err(ApiError::from)?);
        }
        if self.shutting_down.load(Ordering::Acquire) {
            return Err(closed());
        }
        Ok(activity)
    }

    pub(super) async fn protected_context(
        &self,
        id: &str,
    ) -> Result<CacheReply<serde_json::Value>, ApiError> {
        let activity = self
            .cache_activity(false, crate::cache::memory::Work::Bounded)
            .await?;
        let worker = self.worker(id)?;
        let configured = worker
            .spec
            .execution_profile
            .resolved_arguments()
            .map_err(|error| ApiError::new(StatusCode::CONFLICT, error.code, error.message))?
            .1;
        let (revision, applied, reason) = match worker.state.try_lock() {
            Ok(state) => {
                self.ensure_registered(&worker)?;
                let applied = state.running.as_ref().map(|run| serde_json::json!({"source":"spawned argument vector","launch":run.plan.effective(),"launch_plan":run.plan.summary(),"pid":run.child.id(),"execution_fingerprint":run.fingerprint,"runtime_identity":run.runtime_summary.lock().unwrap_or_else(|error| error.into_inner()).clone(),"engine_confirmed":false}));
                (
                    state.revision,
                    applied,
                    state.running.is_none().then_some("worker_unloaded"),
                )
            }
            Err(_) => (
                worker
                    .observation
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .as_ref()
                    .expect("worker observation")
                    .revision,
                None,
                Some("state_operation_busy"),
            ),
        };
        let (declared, observed, reason) = if let Some(reason) = reason {
            (None, None, Some(reason))
        } else {
            let observation = async {
                let (owner, _cancellation) = self
                    .state_operation(
                        worker.clone(),
                        0,
                        "",
                        StateAction::Inspect,
                        self.state_budget(&worker)?,
                        activity.clone(),
                        None,
                        false,
                    )
                    .await?;
                let (mut owner, declared) = owner
                    .blocking(|owner| {
                        let mut file = std::fs::File::open(&owner.worker.spec.model_file)
                            .map_err(|_| state_observation_error())?;
                        crate::profiles::read_gguf(&mut file).map_err(|_| state_observation_error())
                    })
                    .await?;
                owner = owner.verify().await?;
                let (props, slots) = owner.inspect(&self.client).await?;
                Ok::<_, ApiError>((declared, serde_json::json!({"props":props,"slots":slots})))
            }
            .await;
            match observation {
                Ok((declared, observed)) => (Some(declared), Some(observed), None),
                Err(error) => (None, None, Some(error.code)),
            }
        };
        Ok(CacheReply::new(
            serde_json::json!({"schema_version":1,"model":id,"revision":revision,"provider":worker.spec.execution_profile.adapter,"declared":declared,"configured":configured,"applied":applied,"observed":observed,"observation_error":reason,"qualification":"unqualified","note":"Training context is model metadata. Launch context is a total budget; only engine observations describe serving slots. No context is inferred by division."}),
            activity,
        ))
    }

    pub async fn recover_snapshot(
        &self,
        request: CacheRecovery,
        context: RequestContext,
    ) -> Result<CacheReply<serde_json::Value>, ApiError> {
        Self::cache_session(&request.session)?;
        let cache = self.cache_store()?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancellation = Cancellation(cancelled.clone());
        let control = OperationControl::new(Instant::now() + self.request_timeout, cancelled);
        let activity = self
            .cache_activity(false, crate::cache::memory::Work::Bounded)
            .await?;
        tokio::task::spawn_blocking(move || {
            let _activity = activity.clone();
            control.check().map_err(cache_error)?;
            let snapshot = cache
                .observation(
                    &request.snapshot,
                    context.principal,
                    &context.credential_epoch,
                )
                .map_err(cache_error)?;
            let identity = snapshot_identity(
                &snapshot.execution_fingerprint,
                &context,
                &request.session,
                &snapshot.protocol,
            );
            if let Some(reader) = &request.reader {
                return cache
                    .recover_reader(
                        &request.snapshot,
                        reader,
                        &identity,
                        request.expected_revision,
                        &control,
                    )
                    .map(|result| CacheReply::new(serde_json::json!(result), activity))
                    .map_err(cache_error);
            }
            cache
                .recover_writer(
                    &request.snapshot,
                    &identity,
                    request.expected_revision,
                    &control,
                )
                .map(|result| CacheReply::new(serde_json::json!(result), activity))
                .map_err(cache_error)
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache_recovery_interrupted",
                "Snapshot recovery was interrupted. Inspect its retained charge before retrying.",
            )
        })?
    }

    pub(super) fn state_budget(&self, worker: &Worker) -> Result<StateBudget, ApiError> {
        let profile = self.protected_profile(worker)?;
        let (bytes, priority, timeout) = if let Some(profile) = profile {
            let bytes = profile.state_bytes.ok_or_else(|| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "state_work_unqualified",
                    "This protected profile has no state-operation memory bound.",
                )
            })?;
            (
                Some(bytes),
                profile.priority,
                self.request_timeout
                    .min(Duration::from_millis(profile.completion_ms)),
            )
        } else {
            (
                None,
                crate::admission::Priority::Interactive,
                self.request_timeout,
            )
        };
        Ok(StateBudget {
            bytes,
            priority,
            deadline: Instant::now() + timeout,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn state_operation(
        &self,
        worker: Arc<Worker>,
        slot: u32,
        session: &str,
        action: StateAction,
        budget: StateBudget,
        activity: Arc<CacheActivity>,
        lender: Option<ClaimCell>,
        automatic: bool,
    ) -> Result<(StateOperation, Cancellation), ApiError> {
        if automatic {
            Self::automatic_eligible(&worker, slot)?;
        } else if action != StateAction::Inspect {
            Self::cache_eligible(&worker, slot, session)?;
        }
        let StateBudget {
            bytes,
            priority,
            deadline,
        } = budget;
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = Cancellation(cancelled.clone());
        let control = OperationControl::new(deadline, cancelled);
        if action != StateAction::Inspect && !automatic {
            let config = tokio::time::timeout_at(deadline.into(), self.config.lock())
                .await
                .map_err(|_| state_deadline_error())?;
            if !config.cache_enabled {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "cache_disabled",
                    "Enable persistent snapshots before state operations.",
                ));
            }
        }
        let cache = self.cache.clone();
        let model = worker.record_model.clone();
        let preflight = control.clone();
        let activity = tokio::task::spawn_blocking(move || {
            let activity = activity;
            preflight.check().map_err(cache_error)?;
            if let Some(cache) = cache {
                cache
                    .check_state_readers(&model, &preflight)
                    .map_err(cache_error)?;
            }
            Ok::<_, ApiError>(activity)
        })
        .await
        .map_err(|_| state_observation_error())??;
        let lent_claims = lender.is_some();
        let physical_slot = if lent_claims {
            PhysicalSlot::Lent
        } else if let Some(bytes) = bytes {
            self.host_memory.refresh(&self.admission).await?;
            let acquire = worker.slots.acquire_with_memory(priority, bytes);
            tokio::pin!(acquire);
            loop {
                tokio::select! {
                    slot = &mut acquire => break PhysicalSlot::Owned(slot.map_err(ApiError::from)?),
                    _ = tokio::time::sleep_until(deadline.into()) => return Err(state_deadline_error()),
                    _ = tokio::time::sleep(Duration::from_millis(250)) => self.host_memory.refresh(&self.admission).await?,
                }
            }
        } else {
            PhysicalSlot::Owned(worker.slots.try_acquire(priority).map_err(|_| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "worker_busy",
                    "Wait for active requests before a slot state operation.",
                )
            })?)
        };
        let cache = self.cache.clone();
        let model = worker.record_model.clone();
        let preflight = control.clone();
        let (physical_slot, activity) = tokio::task::spawn_blocking(move || {
            let claims = (physical_slot, activity);
            preflight.check().map_err(cache_error)?;
            if let Some(cache) = cache {
                cache
                    .check_state_readers(&model, &preflight)
                    .map_err(cache_error)?;
            }
            Ok::<_, ApiError>(claims)
        })
        .await
        .map_err(|_| state_observation_error())??;
        if action == StateAction::Restore && !automatic {
            tokio::time::timeout_at(deadline.into(), self.load(&worker.spec.id))
                .await
                .map_err(|_| state_deadline_error())??;
        }
        let mut guard = tokio::time::timeout_at(deadline.into(), worker.state.clone().lock_owned())
            .await
            .map_err(|_| state_deadline_error())?;
        self.ensure_registered(&worker)?;
        Self::reap(&mut guard);
        Self::process_available(&worker, guard.running.is_some())?;
        if guard.policy == Policy::Disabled {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "model_disabled",
                "Enable this model before state operations.",
            ));
        }
        let run = guard.running.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "worker_unloaded",
                "Load this worker and complete a session-bound request before saving.",
            )
        })?;
        if action != StateAction::Inspect && !run.plan.has_cache_binding() {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "cache_profile_unsupported",
                "The running generation has no managed snapshot path.",
            ));
        }
        if let Some(profile) = self.protected_profile(&worker)? {
            profile
                .verify_fingerprint(run.plan.fingerprint())
                .map_err(host::profile_error)?;
        }
        let generation = run
            .child
            .resources()
            .state
            .as_ref()
            .ok_or_else(|| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "state_owner_unavailable",
                    "The running generation has no state ownership lane.",
                )
            })?
            .clone();
        control.check().map_err(cache_error)?;
        if self.shutting_down.load(Ordering::Acquire)
            || (self.paused.load(Ordering::Acquire) && !lent_claims)
        {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "admission_closed",
                "Snapshot admission is closed.",
            ));
        }
        let physical = match physical_slot {
            PhysicalSlot::Lent => lender
                .as_ref()
                .and_then(|cell| {
                    cell.lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .take()
                })
                .ok_or_else(|| {
                    ApiError::new(
                        StatusCode::CONFLICT,
                        "worker_busy",
                        "The request slot was released before its state operation.",
                    )
                })?,
            PhysicalSlot::Owned(slot) => {
                let power = self.power.activity(deadline).await;
                worker.active.fetch_add(1, Ordering::AcqRel);
                PhysicalClaims {
                    worker: Arc::downgrade(&worker),
                    _slot: slot,
                    _power: power,
                    uncertain: false,
                }
            }
        };
        Ok((
            StateOperation {
                claims: Some(StateClaims {
                    physical: Some(physical),
                    uncertain: false,
                }),
                worker,
                guard,
                generation,
                file: None,
                exchange_pending: false,
                control,
                _activity: activity,
                lender,
            },
            cancellation,
        ))
    }

    pub async fn save_snapshot(
        &self,
        request: CacheSave,
        context: RequestContext,
    ) -> Result<CacheReply<serde_json::Value>, ApiError> {
        let cache = self.cache_store()?;
        let worker = self.worker(&request.model)?;
        self.refuse_session_owned(&worker)?;
        self.require_space(
            vec![(self.cache_directory(), request.maximum_bytes)],
            "save this snapshot",
        )
        .await?;
        let budget = self.state_budget(&worker)?;
        let (owner, _cancellation) = self
            .state_operation(
                worker,
                request.slot,
                &request.session,
                StateAction::Save,
                budget,
                self.cache_activity(true, crate::cache::memory::Work::Bounded)
                    .await?,
                None,
                false,
            )
            .await?;
        let identity = snapshot_identity(
            &owner.run().fingerprint,
            &context,
            &request.session,
            "openai.completions",
        );
        if owner
            .worker
            .cache_identity
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            != Some(&identity)
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "cache_context_mismatch",
                "The idle slot does not belong to this completed credential/session context.",
            ));
        }
        let (owner, metadata, acknowledgement) = self
            .save_slot(
                owner,
                cache,
                identity,
                request.slot,
                Some(request.expected_generation),
                request.maximum_bytes,
                request.maximum_bytes,
                Vec::new(),
            )
            .await?;
        Ok(CacheReply::new(
            serde_json::json!({"snapshot":metadata,"worker_generation_qualified":false,"saved":true,"engine":acknowledgement}),
            owner._activity.clone(),
        ))
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn save_slot(
        &self,
        owner: StateOperation,
        cache: Arc<CacheStore>,
        identity: SnapshotIdentity,
        slot: u32,
        expected_generation: Option<u64>,
        maximum_bytes: u64,
        growth_ceiling: u64,
        evictions: Vec<(String, SnapshotIdentity, u64)>,
    ) -> Result<
        (
            StateOperation,
            crate::cache::SnapshotMetadata,
            serde_json::Value,
        ),
        ApiError,
    > {
        let roots = self.live_roots.clone();
        let (owner, (cache, identity)) = owner
            .blocking(move |_| {
                for (id, identity, revision) in &evictions {
                    let _ = cache.forget(id, identity, *revision);
                }
                let generation = expected_generation.unwrap_or_else(|| cache.generation());
                cache
                    .check_reservation(&identity, generation, maximum_bytes)
                    .map_err(cache_error)?;
                if let Some(roots) = &roots {
                    roots.verify().map_err(|_| storage_root_error())?;
                }
                Ok((cache, identity))
            })
            .await?;
        let mut owner = owner.verify().await?;
        if self.memory_protected() {
            owner.inspect(&self.client).await?;
        }
        let (owner, writer) = owner
            .blocking(move |owner| {
                let writer = owner.capture_actor()?;
                let mut attempts = 0;
                let pending = loop {
                    let generation = expected_generation.unwrap_or_else(|| cache.generation());
                    match cache.begin_bound(
                        identity.clone(),
                        generation,
                        maximum_bytes,
                        writer.clone(),
                        &owner.control,
                    ) {
                        Err(CacheError::Conflict)
                            if expected_generation.is_none() && attempts < 4 =>
                        {
                            attempts += 1;
                        }
                        result => break result.map_err(cache_error)?,
                    }
                };
                owner.file = Some(FileOwner::Pending(pending));
                Ok((cache, writer))
            })
            .await?;
        let (cache, writer) = writer;
        let roots = self.live_roots.clone();
        let validator = cache.clone();
        let (mut owner, filename) = owner
            .blocking(move |owner| {
                if let Some(roots) = &roots {
                    roots.verify().map_err(|_| storage_root_error())?;
                }
                owner.validate_actor(&writer)?;
                let Some(FileOwner::Pending(pending)) = &owner.file else {
                    unreachable!()
                };
                validator
                    .validate_writer(pending, &writer, &owner.control)
                    .map_err(cache_error)?;
                Ok(pending
                    .path()
                    .file_name()
                    .expect("generated snapshot filename")
                    .to_string_lossy()
                    .into_owned())
            })
            .await?;
        let base = owner.run().base.clone();
        let (status, bytes) = owner
            .exchange(
                &self.client,
                self.client
                    .post(format!("{base}/slots/{slot}?action=save"))
                    .json(&serde_json::json!({"filename":filename})),
            )
            .await?;
        if !status.is_success() {
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "cache_save_rejected",
                "The engine rejected the slot save. Its staging reservation remains retained.",
            ));
        }
        let acknowledgement = acknowledgement(&bytes, &filename, slot, "n_saved", "n_written")?;
        owner = owner.verify().await?;
        let (owner, metadata) = owner
            .blocking(move |owner| {
                let Some(FileOwner::Pending(pending)) = &owner.file else {
                    unreachable!()
                };
                cache
                    .publish_within(pending, pending.revision(), &owner.control, growth_ceiling)
                    .map_err(cache_error)
            })
            .await?;
        Ok((owner, metadata, acknowledgement))
    }

    pub async fn restore_snapshot(
        &self,
        request: CacheRestore,
        context: RequestContext,
    ) -> Result<CacheReply<serde_json::Value>, ApiError> {
        Self::cache_session(&request.session)?;
        let cache = self.cache_store()?;
        let worker = self.worker(&request.model)?;
        Self::cache_eligible(&worker, request.slot, &request.session)?;
        self.refuse_session_owned(&worker)?;
        let budget = self.state_budget(&worker)?;
        let observer = cache.clone();
        let id = request.snapshot.clone();
        let principal = context.principal;
        let epoch = context.credential_epoch.clone();
        let activity = self
            .cache_activity(true, crate::cache::memory::Work::Bounded)
            .await?;
        let read_activity = activity.clone();
        let snapshot = tokio::time::timeout_at(
            budget.deadline.into(),
            tokio::task::spawn_blocking(move || {
                let _activity = read_activity;
                observer.observation(&id, principal, &epoch)
            }),
        )
        .await
        .map_err(|_| state_deadline_error())?
        .map_err(|_| state_observation_error())?
        .map_err(cache_error)?;
        let expected = snapshot_identity(
            &snapshot.execution_fingerprint,
            &context,
            &request.session,
            "openai.completions",
        );
        if snapshot.session_digest != expected.session_digest
            || snapshot.protocol != expected.protocol
        {
            return Err(cache_error(CacheError::IdentityMismatch));
        }
        if snapshot.state != crate::cache::SnapshotState::Saved {
            return Err(cache_error(CacheError::Uncertain));
        }
        let (owner, _cancellation) = self
            .state_operation(
                worker,
                request.slot,
                &request.session,
                StateAction::Restore,
                budget,
                activity,
                None,
                false,
            )
            .await?;
        let identity = snapshot_identity(
            &owner.run().fingerprint,
            &context,
            &request.session,
            "openai.completions",
        );
        let (owner, metadata, acknowledgement, settlement) = self
            .restore_slot(
                owner,
                cache,
                request.snapshot,
                identity,
                request.slot,
                false,
            )
            .await?;
        Ok(CacheReply::new(
            serde_json::json!({"restored":true,"snapshot":metadata,"qualification":"unqualified","model":request.model,"engine":acknowledgement,"retained_reader_cleanup_pending":settlement.journal_cleanup_pending}),
            owner._activity.clone(),
        ))
    }

    pub(super) async fn restore_slot(
        &self,
        owner: StateOperation,
        cache: Arc<CacheStore>,
        snapshot: String,
        identity: SnapshotIdentity,
        slot: u32,
        automatic: bool,
    ) -> Result<
        (
            StateOperation,
            crate::cache::SnapshotMetadata,
            serde_json::Value,
            crate::cache::ReaderSettlement,
        ),
        ApiError,
    > {
        let expected = identity.clone();
        let roots = self.live_roots.clone();
        let mut owner = owner
            .blocking(move |_| {
                if let Some(roots) = &roots {
                    roots.verify().map_err(|_| storage_root_error())?;
                }
                Ok(())
            })
            .await?
            .0;
        owner = owner.verify().await?;
        if self.memory_protected() {
            owner.inspect(&self.client).await?;
        }
        let (owner, (filename, writer, cache)) = owner
            .blocking(move |owner| {
                let lease = cache
                    .restore_controlled(&snapshot, &expected, &owner.control)
                    .map_err(cache_error)?;
                let writer = owner.capture_actor()?;
                let lease = cache
                    .bind_reader(lease, writer.clone(), &owner.control)
                    .map_err(cache_error)?;
                let filename = lease
                    .path()
                    .file_name()
                    .expect("generated snapshot filename")
                    .to_string_lossy()
                    .into_owned();
                owner.file = Some(FileOwner::Reader(lease));
                Ok((filename, writer, cache))
            })
            .await?;
        let owner = owner.verify().await?;
        *owner
            .worker
            .cache_identity
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = None;
        let roots = self.live_roots.clone();
        let (mut owner, cache) = owner
            .blocking(move |owner| {
                if let Some(roots) = &roots {
                    roots.verify().map_err(|_| storage_root_error())?;
                }
                owner.validate_actor(&writer)?;
                let Some(FileOwner::Reader(lease)) = &owner.file else {
                    unreachable!()
                };
                cache
                    .validate_reader(lease, &writer, &owner.control)
                    .map_err(cache_error)?;
                Ok(cache)
            })
            .await?;
        let base = owner.run().base.clone();
        let (status, bytes) = owner
            .exchange(
                &self.client,
                self.client
                    .post(format!("{base}/slots/{slot}?action=restore"))
                    .json(&serde_json::json!({"filename":filename})),
            )
            .await?;
        if !status.is_success() {
            if automatic {
                let _ = owner
                    .blocking(move |owner| {
                        if let Some(FileOwner::Reader(lease)) = &owner.file {
                            let _ = cache.settle_reader(lease, &owner.control);
                        }
                        Ok(())
                    })
                    .await;
            }
            return Err(ApiError::new(
                StatusCode::BAD_GATEWAY,
                "cache_restore_rejected",
                "The engine rejected this snapshot. Rebuild the context cold.",
            ));
        }
        let acknowledgement = acknowledgement(&bytes, &filename, slot, "n_restored", "n_read")?;
        let (owner, (identity, metadata, settlement)) = owner
            .blocking(move |owner| {
                let Some(FileOwner::Reader(lease)) = &owner.file else {
                    unreachable!()
                };
                let settlement = cache
                    .settle_reader(lease, &owner.control)
                    .map_err(cache_error)?;
                let metadata = cache
                    .observation(
                        &lease.metadata().id,
                        &identity.principal,
                        &identity.credential_epoch,
                    )
                    .map_err(cache_error)?;
                Ok((identity, metadata, settlement))
            })
            .await?;
        let owner = owner.verify().await?;
        if !automatic {
            *owner
                .worker
                .cache_identity
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(identity);
        }
        Ok((owner, metadata, acknowledgement, settlement))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn owner(manager: &Arc<Manager>) -> (StateOperation, Cancellation, Arc<RetainedStates>) {
        let worker = manager.worker("one").unwrap();
        let claims = super::super::tests::speech_claims(manager, &worker).await;
        let physical = claims.0.lock().unwrap().take().unwrap();
        let guard = worker.state.clone().lock_owned().await;
        let generation = Arc::new(RetainedStates::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let cancellation = Cancellation(cancelled.clone());
        let activity = manager.cache_operations.enter(true);
        (
            StateOperation {
                worker,
                guard,
                claims: Some(StateClaims {
                    physical: Some(physical),
                    uncertain: false,
                }),
                generation: generation.clone(),
                file: None,
                exchange_pending: false,
                control: OperationControl::new(Instant::now() + Duration::from_secs(10), cancelled),
                _activity: activity,
                lender: None,
            },
            cancellation,
            generation,
        )
    }

    fn counts(manager: &Manager, worker: &Worker, active: u32, uncertain: u32) {
        assert_eq!(worker.active.load(Ordering::Acquire), active);
        assert_eq!(
            worker.uncertain_state_operations.load(Ordering::Acquire),
            uncertain
        );
        assert_eq!(worker.uncertain_requests.load(Ordering::Acquire), 0);
        assert_eq!(worker.slots.snapshot().active, active);
        assert_eq!(manager.power.snapshot().active, active as usize);
    }

    #[tokio::test]
    async fn predispatch_refunds_and_uncertain_state_retains_separately_without_lock_or_task() {
        let (_directory, manager) = super::super::tests::manager();
        let worker = manager.worker("one").unwrap();
        let (operation, _cancel, generation) = owner(&manager).await;
        counts(&manager, &worker, 1, 0);
        drop(operation);
        counts(&manager, &worker, 0, 0);
        assert!(generation.0.lock().unwrap().is_none());
        let (mut operation, _cancel, generation) = owner(&manager).await;
        operation.exchange_pending = true;
        drop(operation);
        counts(&manager, &worker, 1, 1);
        assert!(worker.state.try_lock().is_ok());
        assert!(generation.0.lock().unwrap().is_some());
        drop(generation);
        counts(&manager, &worker, 0, 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn caller_cancellation_retains_physical_claims_and_serialization_through_actual_blocking_work()
     {
        let (_directory, manager) = super::super::tests::manager();
        let worker = manager.worker("one").unwrap();
        let (operation, cancellation, _generation) = owner(&manager).await;
        let (entered, wait) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let task = tokio::spawn(operation.blocking(move |_| {
            entered.send(()).unwrap();
            blocked.recv().unwrap();
            Ok(())
        }));
        wait.await.unwrap();
        task.abort();
        let _ = task.await;
        drop(cancellation);
        counts(&manager, &worker, 1, 0);
        assert!(worker.state.try_lock().is_err());
        assert_eq!(manager.cache_operations.active(), 1);
        assert!(
            !manager
                .cache_operations
                .drain(tokio::time::Instant::now())
                .await
        );
        release.send(()).unwrap();
        let _guard = tokio::time::timeout(Duration::from_secs(2), worker.state.lock())
            .await
            .unwrap();
        counts(&manager, &worker, 0, 0);
        assert!(
            manager
                .cache_operations
                .drain(tokio::time::Instant::now())
                .await
        );
    }

    #[tokio::test]
    async fn original_error_eof_settles_but_interrupted_next_exchange_stays_uncertain() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (_directory, manager) = super::super::tests::manager();
        let worker = manager.worker("one").unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            for response in [b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".as_slice(),b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\n{}".as_slice()] {
                let (mut socket,_) = listener.accept().await.unwrap();
                let mut request=[0;1024];
                let _ = socket.read(&mut request).await.unwrap();
                socket.write_all(response).await.unwrap();
                socket.shutdown().await.unwrap();
            }
        });
        let (mut operation, _cancel, generation) = owner(&manager).await;
        let url = format!("http://{address}/state");
        let (status, body) = operation
            .exchange(&manager.client, manager.client.get(&url))
            .await
            .unwrap();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body, b"{}");
        assert!(!operation.exchange_pending);
        assert!(
            operation
                .exchange(&manager.client, manager.client.get(&url))
                .await
                .is_err()
        );
        assert!(operation.exchange_pending);
        drop(operation);
        server.await.unwrap();
        counts(&manager, &worker, 1, 1);
        drop(generation);
        counts(&manager, &worker, 0, 0);
    }

    #[cfg(unix)]
    #[test]
    fn state_acknowledgements_skip_dense_extras_and_require_exact_unambiguous_evidence() {
        let padding = "0,".repeat(400_000) + "0";
        let reply = format!(
            "{{\"id_slot\":0,\"filename\":\"state.bin\",\"n_saved\":5,\"n_written\":64,\"unknown\":[{padding}]}}"
        );
        assert_eq!(
            acknowledgement(reply.as_bytes(), "state.bin", 0, "n_saved", "n_written").unwrap(),
            serde_json::json!({"id_slot":0,"n_saved":5,"n_written":64})
        );
        for reply in [
            br#"{"id_slot":0,"id_slot":1,"filename":"state.bin","n_saved":5,"n_written":64}"#
                .as_slice(),
            br#"{"id_slot":0,"filename":"other.bin","n_saved":5,"n_written":64}"#.as_slice(),
            br#"{"id_slot":0,"filename":"state.bin","n_saved":0,"n_written":64}"#.as_slice(),
            br#"{"id_slot":0,"filename":"state.bin","n_saved":5,"n_written":1.5}"#.as_slice(),
            br#"{"id_slot":0,"filename":"state.bin","n_saved":[1,2,3],"n_written":64}"#.as_slice(),
        ] {
            assert!(acknowledgement(reply, "state.bin", 0, "n_saved", "n_written").is_err());
        }
        let filename = format!(
            "{{\"id_slot\":0,\"filename\":\"{}\",\"n_saved\":5,\"n_written\":64}}",
            "x".repeat(900_000)
        );
        assert!(
            acknowledgement(filename.as_bytes(), "state.bin", 0, "n_saved", "n_written").is_err()
        );
    }

    #[tokio::test]
    async fn failed_stop_and_unconfirmed_exit_keep_state_claims_until_exact_acknowledgement() {
        let (_directory, manager) = super::super::tests::manager();
        let worker = manager.worker("one").unwrap();
        let (mut operation, _cancel, _unused) = owner(&manager).await;
        let run =
            super::super::tests::speech_child(&manager, &worker, Arc::new(SpeechClaims::default()))
                .await;
        let generation = run.child.generation();
        operation.generation = run.child.resources().state.as_ref().unwrap().clone();
        operation.guard.running = Some(run);
        operation.guard.policy = Policy::Hot;
        operation.exchange_pending = true;
        drop(operation);
        counts(&manager, &worker, 1, 1);
        assert_eq!(manager.catalog().await[0].state, "cleanup-pending");
        assert_eq!(manager.catalog().await[0].uncertain_requests, 0);
        manager.processes.test_fail_kill(true);
        assert!(manager.unload("one").await.is_err());
        counts(&manager, &worker, 1, 1);
        assert_eq!(
            manager.processes.snapshot("one").unwrap().generation,
            generation
        );
        manager.processes.test_fail_kill(false);
        manager.processes.test_withhold_ack(true);
        assert!(manager.unload("one").await.is_err());
        counts(&manager, &worker, 1, 1);
        manager.processes.test_withhold_ack(false);
        let view = manager.unload("one").await.unwrap();
        assert_eq!(view.policy, Policy::Hot);
        assert_eq!(view.uncertain_state_operations, 0);
        counts(&manager, &worker, 0, 0);
        assert!(manager.processes.snapshot("one").is_none());
    }
}
