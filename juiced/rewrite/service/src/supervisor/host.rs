use super::*;
use crate::config::Provider;

pub(super) struct HostMemory {
    gate: Arc<AsyncMutex<()>>,
    observation: Arc<RwLock<Option<Arc<crate::host_memory::Observation>>>>,
    closing: AtomicBool,
}

impl HostMemory {
    pub(super) fn new() -> Self {
        Self {
            gate: Arc::new(AsyncMutex::new(())),
            observation: Arc::new(RwLock::new(None)),
            closing: AtomicBool::new(false),
        }
    }

    pub(super) fn sample_bootstrap(&self, admission: &Arc<crate::admission::Admission>) {
        Self::capture(admission, &self.observation);
    }

    fn capture(
        admission: &crate::admission::Admission,
        published: &RwLock<Option<Arc<crate::host_memory::Observation>>>,
    ) {
        let observation = Arc::new(crate::host_memory::sample(
            Instant::now() + Duration::from_millis(200),
        ));
        admission.publish_host_sample(&observation);
        *published.write().unwrap_or_else(|error| error.into_inner()) = Some(observation);
    }

    pub(super) async fn refresh(
        &self,
        admission: &Arc<crate::admission::Admission>,
    ) -> Result<(), ApiError> {
        self.available()?;
        if !admission.needs_host_sample() {
            return Ok(());
        }
        let gate = self.gate.clone().lock_owned().await;
        self.available()?;
        if !admission.needs_host_sample() {
            return Ok(());
        }
        let admission = admission.clone();
        let published = self.observation.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = gate;
            Self::capture(&admission, &published);
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "host_sample_interrupted",
                "The host memory observation did not settle. Capacity remains unavailable.",
            )
        })
    }

    pub(super) fn observation(&self) -> Option<Arc<crate::host_memory::Observation>> {
        self.observation
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(super) async fn settled(&self, deadline: tokio::time::Instant) -> bool {
        self.closing.store(true, Ordering::Release);
        tokio::time::timeout_at(deadline, self.gate.lock())
            .await
            .is_ok()
    }

    fn available(&self) -> Result<(), ApiError> {
        if self.closing.load(Ordering::Acquire) {
            Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "host_sample_stopping",
                "Host memory sampling is closed while the service is stopping.",
            ))
        } else {
            Ok(())
        }
    }
}

impl Manager {
    pub(super) fn protected_profile<'a>(
        &self,
        worker: &'a Worker,
    ) -> Result<Option<&'a crate::resource_profile::Profile>, ApiError> {
        if !self.admission.memory_protected() {
            return Ok(None);
        }
        if worker.physical_invariant_failed.load(Ordering::Acquire) {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "physical_claim_invariant",
                "The retained physical ownership invariant failed. Preserve this owner and its resources for recovery.",
            ));
        }
        let profile = worker.spec.resource_profile.as_ref().ok_or_else(|| {
            ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "resource_profile_required",
                "Protected execution requires an exact server-bound resource profile.",
            )
        })?;
        if worker.spec.execution_profile.adapter != Provider::LlamaCpp
            || worker.spec.execution_profile.native_model_id().is_none()
            || worker.spec.artifact.is_none()
            || worker.spec.execution_profile.runtime_bundle().is_none()
            || worker
                .spec
                .execution_profile
                .options()
                .is_none_or(|options| {
                    options.gpu_layers != Some(0)
                        || options.parallel_slots != Some(worker.spec.max_inflight)
                        || !(1..=crate::profiles::MAX_PARALLEL_SLOTS)
                            .contains(&worker.spec.max_inflight)
                })
        {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "protected_profile_unqualified",
                "The selected protected path requires a registered imported CPU llama.cpp model, a verified bundle and matching declared physical slots.",
            ));
        }
        Ok(Some(profile))
    }

    pub(super) async fn reserve_load_memory(
        &self,
        worker: &Worker,
    ) -> Result<Option<crate::admission::MemoryReservation>, ApiError> {
        let Some(profile) = self.protected_profile(worker)? else {
            return Ok(None);
        };
        self.host_memory.refresh(&self.admission).await?;
        self.admission
            .reserve_host(profile.load_bytes, profile.priority)
            .map(Some)
            .map_err(ApiError::from)
    }
}

pub(crate) struct PreparedRequest {
    worker: Arc<Worker>,
    revision: u64,
    operation: Operation,
    body: axum::body::Bytes,
}

pub(super) fn profile_error(error: crate::resource_profile::Error) -> ApiError {
    ApiError::new(
        StatusCode::from_u16(error.status).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
        error.code,
        error.message,
    )
}

impl Manager {
    pub(crate) fn memory_protected(&self) -> bool {
        self.admission.memory_protected()
    }

    pub(crate) fn prepare_protected_request(
        &self,
        id: &str,
        operation: Operation,
        body: &axum::body::Bytes,
    ) -> Result<PreparedRequest, ApiError> {
        let worker = self.worker(id)?;
        let revision = {
            let observation = worker
                .observation
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            let model = observation.as_ref().expect("worker observation");
            if model.policy == Policy::Disabled {
                return Err(ApiError::new(
                    StatusCode::CONFLICT,
                    "model_disabled",
                    "Enable this model's loading policy before using it.",
                ));
            }
            if !worker.spec.operations.contains(&operation) {
                return Err(ApiError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "unsupported_operation",
                    "This model does not declare support for the requested operation.",
                ));
            }
            model.revision
        };
        let profile = self.protected_profile(&worker)?.ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "protected_mode_required",
                "This request binding requires protected host admission.",
            )
        })?;
        let value = serde_json::from_slice(body).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "Send a valid bounded JSON request.",
            )
        })?;
        let envelope = profile.resolve(operation, value).map_err(profile_error)?;
        if envelope.workload.batch > worker.spec.max_inflight {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "physical_batch_unqualified",
                "The requested batch exceeds the selected physical slot profile.",
            ));
        }
        let body = axum::body::Bytes::from(serde_json::to_vec(&envelope.body).map_err(|_| {
            ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "The normalized text request could not be encoded.",
            )
        })?);
        Ok(PreparedRequest {
            worker,
            revision,
            operation,
            body,
        })
    }

    pub(crate) async fn begin_protected_request(
        self: &Arc<Self>,
        prepared: PreparedRequest,
    ) -> Result<(String, RequestLease, axum::body::Bytes), ApiError> {
        let profile = self
            .protected_profile(&prepared.worker)?
            .expect("protected request carries a qualified profile");
        self.host_memory.refresh(&self.admission).await?;
        let started = Instant::now();
        let slot = {
            let acquire = prepared
                .worker
                .slots
                .acquire_with_memory(profile.priority, profile.request_bytes);
            tokio::pin!(acquire);
            loop {
                tokio::select! {
                    slot = &mut acquire => break slot.map_err(ApiError::from)?,
                    _ = tokio::time::sleep(Duration::from_millis(250)) => self.host_memory.refresh(&self.admission).await?,
                }
            }
        };
        let (base, lease) = self
            .begin_admitted(
                prepared.worker,
                prepared.operation,
                slot,
                started,
                Some(prepared.revision),
                started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            )
            .await?;
        Ok((base, lease, prepared.body))
    }
}

pub(crate) struct IngressClaims {
    _body: crate::admission::IngressLease,
    _host: Option<crate::admission::MemoryReservation>,
}

impl Manager {
    pub(crate) async fn reserve_http_ingress(
        &self,
        maximum: usize,
        declared_bytes: Option<u64>,
    ) -> Result<IngressClaims, ApiError> {
        let body = self.reserve_ingress(maximum)?;
        let host = if self.memory_protected() {
            self.host_memory.refresh(&self.admission).await?;
            // A short JSON scalar can occupy a full Value and container entry.
            // Charge parser structure as well as transport bytes before parsing.
            let framed_bytes = declared_bytes
                .unwrap_or(maximum as u64)
                .min(maximum as u64)
                .max(1);
            let bytes = framed_bytes.checked_mul(64).ok_or_else(|| {
                ApiError::new(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "ingress_capacity",
                    "The ingress memory ceiling cannot be represented.",
                )
            })?;
            Some(
                self.admission
                    .reserve_host(bytes, crate::admission::Priority::Interactive)
                    .map_err(ApiError::from)?,
            )
        } else {
            None
        };
        Ok(IngressClaims {
            _body: body,
            _host: host,
        })
    }
}

pub(super) struct HostAssessment {
    pub(super) snapshot: crate::admission::MemorySnapshot,
    pub(super) additional: Option<u64>,
    pub(super) available: Option<u64>,
    pub(super) fits: Option<bool>,
    pub(super) blocker: Option<&'static str>,
}

impl Manager {
    pub(super) fn protected_suitability(&self, worker: &Worker) -> Option<HostAssessment> {
        if !self.memory_protected() {
            return None;
        }
        match self.protected_profile(worker) {
            Ok(Some(profile)) => {
                let (snapshot, result) = self
                    .admission
                    .assess_host(profile.load_bytes, profile.priority);
                let headroom = if profile.priority == crate::admission::Priority::Essential {
                    0
                } else {
                    snapshot.essential_headroom_bytes
                };
                let available = snapshot
                    .capacity_bytes
                    .zip(snapshot.sampled_available_bytes)
                    .map(|(capacity, available)| {
                        capacity
                            .saturating_sub(snapshot.reserved_bytes)
                            .saturating_sub(headroom)
                            .min(
                                available
                                    .saturating_sub(snapshot.outstanding_bytes)
                                    .saturating_sub(headroom),
                            )
                    });
                let fits = match &result {
                    Ok(()) => Some(true),
                    Err(error) if error.code == "host_memory_exhausted" => Some(false),
                    _ => None,
                };
                Some(HostAssessment {
                    snapshot,
                    additional: Some(profile.load_bytes),
                    available,
                    fits,
                    blocker: result.err().map(|error| error.code),
                })
            }
            other => Some(HostAssessment {
                snapshot: self
                    .admission
                    .memory_snapshot()
                    .expect("protected host pool"),
                additional: None,
                available: None,
                fits: None,
                blocker: Some(
                    other
                        .err()
                        .map_or("resource_profile_required", |error| error.code),
                ),
            }),
        }
    }
}

#[cfg(test)]
mod sampler_tests {
    use super::*;

    #[tokio::test]
    async fn shutdown_closes_future_sampling_before_reporting_the_sampler_settled() {
        let host = HostMemory::new();
        let admission = crate::admission::Admission::new(Default::default()).unwrap();
        assert!(
            host.settled(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
        );
        assert_eq!(
            host.refresh(&admission).await.unwrap_err().code,
            "host_sample_stopping"
        );
        assert!(host.observation().is_none());
    }
}
