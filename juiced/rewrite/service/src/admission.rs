use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque,
    fmt,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};

mod memory;
pub use memory::{Mode as MemoryMode, Settings as MemorySettings, Snapshot as MemorySnapshot};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub startup: crate::startup::Settings,
    #[serde(skip_serializing_if = "MemorySettings::is_default")]
    pub memory: MemorySettings,
    pub pending_limit: usize,
    pub service_pending_limit: usize,
    pub active_limit: Option<usize>,
    #[serde(default = "default_ingress_body_bytes")]
    pub ingress_body_bytes: usize,
    pub essential_wait_ms: u64,
    pub interactive_wait_ms: u64,
    pub batch_wait_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            startup: Default::default(),
            memory: Default::default(),
            pending_limit: 64,
            service_pending_limit: 16,
            active_limit: None,
            ingress_body_bytes: default_ingress_body_bytes(),
            essential_wait_ms: 2_000,
            interactive_wait_ms: 10_000,
            batch_wait_ms: 30_000,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), Error> {
        self.startup
            .validate()
            .map_err(|error| Error::new(error.status, error.code, error.message))?;
        self.memory.validate()?;
        validate_ingress_body_bytes(self.ingress_body_bytes)?;
        if !(1..=4096).contains(&self.pending_limit)
            || !(1..=4096).contains(&self.service_pending_limit)
            || self
                .active_limit
                .is_some_and(|limit| !(1..=4096).contains(&limit))
            || [
                self.essential_wait_ms,
                self.interactive_wait_ms,
                self.batch_wait_ms,
            ]
            .into_iter()
            .any(|wait| !(1..=300_000).contains(&wait))
        {
            return Err(Error::new(
                400,
                "invalid_admission_settings",
                "Pending and optional active limits must be 1 to 4096; queue waits must be 1 to 300000 milliseconds.",
            ));
        }
        Ok(())
    }

    fn wait(&self, priority: Priority) -> Duration {
        Duration::from_millis(match priority {
            Priority::Essential => self.essential_wait_ms,
            Priority::Interactive => self.interactive_wait_ms,
            Priority::Batch => self.batch_wait_ms,
        })
    }
}

pub const JSON_INGRESS_BYTES: usize = 2 * 1024 * 1024;
pub const SPEECH_INGRESS_BYTES: usize = 8 * 1024 * 1024;
pub const MIN_INGRESS_BODY_BYTES: usize = SPEECH_INGRESS_BYTES;
pub const MAX_INGRESS_BODY_BYTES: usize = 1024 * 1024 * 1024;
pub const fn default_ingress_body_bytes() -> usize {
    128 * 1024 * 1024
}

pub fn validate_ingress_body_bytes(bytes: usize) -> Result<(), Error> {
    if !(MIN_INGRESS_BODY_BYTES..=MAX_INGRESS_BODY_BYTES).contains(&bytes) {
        return Err(Error::new(
            400,
            "invalid_ingress_capacity",
            "ingress_body_bytes must be between 8388608 and 1073741824 bytes.",
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct IngressSnapshot {
    pub capacity_bytes: usize,
    pub reserved_bytes: usize,
    pub available_bytes: usize,
}

pub struct Ingress {
    capacity_bytes: usize,
    reserved_bytes: AtomicUsize,
}

#[must_use = "Dropping the ingress lease releases its body-byte reservation."]
pub struct IngressLease {
    ingress: Arc<Ingress>,
    bytes: usize,
}

impl Ingress {
    pub fn new(capacity_bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            capacity_bytes,
            reserved_bytes: AtomicUsize::new(0),
        })
    }

    pub fn try_reserve(self: &Arc<Self>, bytes: usize) -> Result<IngressLease, Error> {
        let unavailable = || {
            Error::new(
                503,
                "ingress_capacity",
                "The service cannot accept another request body right now. Retry after current requests finish.",
            )
        };
        if bytes == 0 || bytes > self.capacity_bytes {
            return Err(unavailable());
        }
        self.reserved_bytes
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |reserved| {
                reserved
                    .checked_add(bytes)
                    .filter(|next| *next <= self.capacity_bytes)
            })
            .map_err(|_| unavailable())?;
        Ok(IngressLease {
            ingress: self.clone(),
            bytes,
        })
    }

    pub fn view(&self) -> IngressSnapshot {
        let reserved_bytes = self.reserved_bytes.load(Ordering::Acquire);
        IngressSnapshot {
            capacity_bytes: self.capacity_bytes,
            reserved_bytes,
            available_bytes: self.capacity_bytes.saturating_sub(reserved_bytes),
        }
    }
}

impl Drop for IngressLease {
    fn drop(&mut self) {
        self.ingress
            .reserved_bytes
            .fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Essential,
    #[default]
    Interactive,
    Batch,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub status: u16,
    pub code: &'static str,
    pub message: &'static str,
}
impl Error {
    fn new(status: u16, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

#[derive(Clone, Debug, Serialize)]
pub struct PoolSnapshot {
    pub service: Arc<str>,
    pub physical_capacity: Option<u32>,
    pub essential_headroom: u32,
    pub active: u32,
    pub ordinary_active: u32,
    pub pending: usize,
    pub closed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub settings: Settings,
    pub paused: bool,
    pub shutting_down: bool,
    pub active: usize,
    pub pending: usize,
    pub essential_streak: u8,
    pub pools: Vec<PoolSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemorySnapshot>,
}

#[derive(Default)]
struct State {
    paused: bool,
    shutting_down: bool,
    active: usize,
    essential_streak: u8,
    pending: VecDeque<Arc<Waiter>>,
    pools: Vec<Weak<Pool>>,
    memory: memory::Ledger,
}

pub struct Admission {
    settings: Settings,
    state: Mutex<State>,
}

struct Pool {
    admission: Arc<Admission>,
    service: Arc<str>,
    capacity: Option<u32>,
    headroom: u32,
    active: AtomicU32,
    ordinary_active: AtomicU32,
    closed: AtomicBool,
}

#[derive(Clone)]
pub struct SlotPool {
    pool: Arc<Pool>,
}

#[must_use = "Dropping a slot releases its physical execution claim."]
pub struct Slot {
    pool: Arc<Pool>,
    priority: Priority,
    memory: Option<memory::Allocation>,
}

#[must_use = "Retain this host claim until preparation or the exact owned process settles."]
pub struct MemoryReservation {
    admission: Arc<Admission>,
    allocation: Option<memory::Allocation>,
}

const WAITING: u8 = 0;
const GRANTED: u8 = 1;
const TIMED_OUT: u8 = 2;
const PAUSED: u8 = 3;
const STOPPED: u8 = 4;
const CLOSED: u8 = 5;
const CANCELLED: u8 = 6;
const MEMORY_UNAVAILABLE: u8 = 7;

struct Waiter {
    pool: Arc<Pool>,
    priority: Priority,
    memory_bytes: Option<u64>,
    deadline: Instant,
    status: AtomicU8,
    changed: Notify,
}

struct Waiting {
    waiter: Arc<Waiter>,
    armed: bool,
}

impl Admission {
    pub fn new(settings: Settings) -> Result<Arc<Self>, Error> {
        settings.validate()?;
        Ok(Arc::new(Self {
            settings,
            state: Mutex::new(State::default()),
        }))
    }

    pub fn pool(
        self: &Arc<Self>,
        service: impl Into<Arc<str>>,
        physical_capacity: Option<u32>,
        essential_headroom: u32,
    ) -> Result<SlotPool, Error> {
        let service = service.into();
        if service.is_empty()
            || service.len() > 256
            || service.chars().any(char::is_control)
            || physical_capacity.is_some_and(|capacity| capacity > 65536)
            || essential_headroom > physical_capacity.unwrap_or(0)
        {
            return Err(Error::new(
                400,
                "invalid_slot_pool",
                "Provide a bounded service ID, at most 65536 physical slots, and headroom within the known capacity.",
            ));
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.pools.retain(|pool| pool.strong_count() != 0);
        if state.pools.iter().filter_map(Weak::upgrade).any(|pool| {
            pool.service == service
                && (!pool.closed.load(Ordering::Relaxed)
                    || pool.active.load(Ordering::Relaxed) != 0)
        }) {
            return Err(Error::new(
                409,
                "duplicate_slot_pool",
                "The service already has an open or active physical slot pool.",
            ));
        }
        let pool = Arc::new(Pool {
            admission: self.clone(),
            service,
            capacity: physical_capacity,
            headroom: essential_headroom,
            active: AtomicU32::new(0),
            ordinary_active: AtomicU32::new(0),
            closed: AtomicBool::new(false),
        });
        state.pools.push(Arc::downgrade(&pool));
        Ok(SlotPool { pool })
    }

    pub fn memory_protected(&self) -> bool {
        self.settings.memory.mode == MemoryMode::Protected
    }

    pub fn memory_snapshot(&self) -> Option<MemorySnapshot> {
        self.memory_protected().then(|| {
            self.state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .memory
                .snapshot(&self.settings.memory, std::time::Instant::now())
        })
    }

    pub fn assess_host(
        &self,
        bytes: u64,
        priority: Priority,
    ) -> (MemorySnapshot, Result<(), Error>) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let now = std::time::Instant::now();
        (
            state.memory.snapshot(&self.settings.memory, now),
            state
                .memory
                .check(&self.settings.memory, bytes, priority, now),
        )
    }

    pub fn needs_host_sample(&self) -> bool {
        self.memory_protected()
            && self
                .settings
                .memory
                .capacity_bytes
                .is_some_and(|capacity| capacity != 0)
            && !self
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .memory
                .fresh(&self.settings.memory, std::time::Instant::now())
    }

    pub fn publish_host_sample(&self, observation: &crate::host_memory::Observation) {
        self.publish_sample(observation.available_bytes(), observation.observed_at());
    }

    #[cfg(test)]
    pub(crate) fn seed_host_sample_for_test(&self, available: Option<u64>) {
        self.publish_sample(available, std::time::Instant::now());
    }

    fn publish_sample(&self, available: Option<u64>, observed_at: std::time::Instant) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.memory.publish(available, observed_at);
        self.drive(&mut state);
    }

    fn check_memory(
        &self,
        state: &State,
        bytes: Option<u64>,
        priority: Priority,
    ) -> Result<(), Error> {
        if !self.memory_protected() {
            return Ok(());
        }
        state.memory.check(
            &self.settings.memory,
            bytes.ok_or_else(memory_profile_required)?,
            priority,
            std::time::Instant::now(),
        )
    }

    fn check_memory_admission(
        &self,
        state: &State,
        bytes: Option<u64>,
        priority: Priority,
    ) -> Result<(), Error> {
        match self.check_memory(state, bytes, priority) {
            Err(error) if error.code == "host_memory_exhausted" => Ok(()),
            other => other,
        }
    }

    fn granted_memory(&self, bytes: Option<u64>) -> Option<memory::Allocation> {
        self.memory_protected().then(|| {
            memory::Allocation::outstanding(
                bytes.expect("granted protected work has a memory ceiling"),
            )
        })
    }

    pub fn reserve_host(
        self: &Arc<Self>,
        bytes: u64,
        priority: Priority,
    ) -> Result<MemoryReservation, Error> {
        self.reserve_host_inner(bytes, priority, false)
    }

    pub(crate) fn reserve_control_host(
        self: &Arc<Self>,
        bytes: u64,
        priority: Priority,
    ) -> Result<MemoryReservation, Error> {
        self.reserve_host_inner(bytes, priority, true)
    }

    fn reserve_host_inner(
        self: &Arc<Self>,
        bytes: u64,
        priority: Priority,
        control: bool,
    ) -> Result<MemoryReservation, Error> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.shutting_down {
            return Err(Self::status_error(STOPPED));
        }
        if state.paused && !control {
            return Err(Self::status_error(PAUSED));
        }
        let allocation = if self.memory_protected() {
            Some(state.memory.reserve(
                &self.settings.memory,
                bytes,
                priority,
                std::time::Instant::now(),
            )?)
        } else {
            None
        };
        Ok(MemoryReservation {
            admission: self.clone(),
            allocation,
        })
    }

    pub fn pause(&self, paused: bool) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.paused = paused;
        if paused {
            Self::reject_waiters(&mut state, PAUSED, |_| true);
        }
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.shutting_down = true;
        Self::reject_waiters(&mut state, STOPPED, |_| true);
    }

    pub fn pending_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .pending
            .len()
    }

    pub fn snapshot(&self) -> Snapshot {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.pools.retain(|pool| pool.strong_count() != 0);
        let pools = state
            .pools
            .iter()
            .filter_map(Weak::upgrade)
            .map(|pool| self.pool_snapshot(&state, &pool))
            .collect();
        Snapshot {
            settings: self.settings.clone(),
            paused: state.paused,
            shutting_down: state.shutting_down,
            active: state.active,
            pending: state.pending.len(),
            essential_streak: state.essential_streak,
            pools,
            memory: (self.settings.memory.mode == MemoryMode::Protected).then(|| {
                state
                    .memory
                    .snapshot(&self.settings.memory, std::time::Instant::now())
            }),
        }
    }

    fn pool_snapshot(&self, state: &State, pool: &Arc<Pool>) -> PoolSnapshot {
        PoolSnapshot {
            service: pool.service.clone(),
            physical_capacity: pool.capacity,
            essential_headroom: pool.headroom,
            active: pool.active.load(Ordering::Relaxed),
            ordinary_active: pool.ordinary_active.load(Ordering::Relaxed),
            pending: state
                .pending
                .iter()
                .filter(|entry| Arc::ptr_eq(&entry.pool, pool))
                .count(),
            closed: pool.closed.load(Ordering::Relaxed),
        }
    }

    fn status_error(status: u8) -> Error {
        match status {
            TIMED_OUT => Error::new(
                504,
                "queue_timeout",
                "The request's queue wait expired before a physical slot became available.",
            ),
            PAUSED => Error::new(
                503,
                "admission_paused",
                "New work is paused for maintenance.",
            ),
            STOPPED => Error::new(503, "shutting_down", "The service is shutting down."),
            MEMORY_UNAVAILABLE => Error::new(
                503,
                "unknown_host_capacity",
                "The queued host memory observation became unavailable. Refresh and retry.",
            ),
            CLOSED => Error::new(
                409,
                "worker_closed",
                "The selected worker's physical slot pool has been retired.",
            ),
            _ => Error::new(
                499,
                "request_cancelled",
                "The waiting request was cancelled.",
            ),
        }
    }

    fn check_open(state: &State, pool: &Pool) -> Result<(), Error> {
        if state.shutting_down {
            return Err(Self::status_error(STOPPED));
        }
        if state.paused {
            return Err(Self::status_error(PAUSED));
        }
        if pool.closed.load(Ordering::Relaxed) {
            return Err(Self::status_error(CLOSED));
        }
        if pool.capacity.is_none() {
            return Err(Error::new(
                503,
                "unknown_slots",
                "The worker's physical execution capacity is unknown.",
            ));
        }
        Ok(())
    }

    fn available(
        &self,
        state: &State,
        pool: &Pool,
        priority: Priority,
        memory_bytes: Option<u64>,
    ) -> bool {
        !state.paused
            && self.check_memory(state, memory_bytes, priority).is_ok()
            && !state.shutting_down
            && !pool.closed.load(Ordering::Relaxed)
            && self
                .settings
                .active_limit
                .is_none_or(|limit| state.active < limit)
            && pool.capacity.is_some_and(|capacity| {
                pool.active.load(Ordering::Relaxed) < capacity
                    && (priority == Priority::Essential
                        || pool.ordinary_active.load(Ordering::Relaxed) < capacity - pool.headroom)
            })
    }

    fn reserve(
        &self,
        state: &mut State,
        pool: &Pool,
        priority: Priority,
        memory_bytes: Option<u64>,
    ) -> Result<Option<memory::Allocation>, Error> {
        let memory = if self.settings.memory.mode == MemoryMode::Protected {
            Some(state.memory.reserve(
                &self.settings.memory,
                memory_bytes.ok_or_else(memory_profile_required)?,
                priority,
                std::time::Instant::now(),
            )?)
        } else {
            None
        };
        state.active += 1;
        pool.active.fetch_add(1, Ordering::Relaxed);
        if priority == Priority::Essential {
            state.essential_streak = state.essential_streak.saturating_add(1).min(8);
        } else {
            pool.ordinary_active.fetch_add(1, Ordering::Relaxed);
            state.essential_streak = 0;
        }
        Ok(memory)
    }

    fn release(
        state: &mut State,
        pool: &Pool,
        priority: Priority,
        memory: Option<memory::Allocation>,
    ) {
        if let Some(allocation) = memory {
            state.memory.release(allocation);
        }
        state.active -= 1;
        pool.active.fetch_sub(1, Ordering::Relaxed);
        if priority != Priority::Essential {
            pool.ordinary_active.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn reject_waiters(state: &mut State, status: u8, selected: impl Fn(&Waiter) -> bool) {
        state.pending.retain(|waiter| {
            if selected(waiter) {
                waiter.status.store(status, Ordering::Release);
                waiter.changed.notify_one();
                false
            } else {
                true
            }
        });
    }

    fn drive(&self, state: &mut State) {
        let now = Instant::now();
        Self::reject_waiters(state, TIMED_OUT, |waiter| waiter.deadline <= now);
        loop {
            let mut essential = None;
            let mut ordinary = None;
            for (index, waiter) in state.pending.iter().enumerate() {
                if !self.available(state, &waiter.pool, waiter.priority, waiter.memory_bytes) {
                    continue;
                }
                if waiter.priority == Priority::Essential {
                    if essential.is_none() {
                        essential = Some(index);
                    }
                } else if ordinary.is_none() {
                    ordinary = Some(index);
                }
            }
            let selected = if state.essential_streak < 8 || ordinary.is_none() {
                essential.or(ordinary)
            } else {
                ordinary.or(essential)
            };
            let Some(index) = selected else {
                break;
            };
            let waiter = state.pending.remove(index).expect("selected waiter");
            if self
                .reserve(state, &waiter.pool, waiter.priority, waiter.memory_bytes)
                .is_err()
            {
                waiter.status.store(MEMORY_UNAVAILABLE, Ordering::Release);
            } else {
                waiter.status.store(GRANTED, Ordering::Release);
            }
            waiter.changed.notify_one();
        }
    }
}

impl SlotPool {
    pub fn in_use(&self) -> bool {
        let state = self
            .pool
            .admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.pool.active.load(Ordering::Acquire) != 0
            || state
                .pending
                .iter()
                .any(|entry| Arc::ptr_eq(&entry.pool, &self.pool))
    }

    pub fn try_acquire(&self, priority: Priority) -> Result<Slot, Error> {
        self.try_acquire_inner(priority, None)
    }

    pub fn try_acquire_with_memory(&self, priority: Priority, bytes: u64) -> Result<Slot, Error> {
        self.try_acquire_inner(priority, Some(bytes))
    }

    fn try_acquire_inner(
        &self,
        priority: Priority,
        memory_bytes: Option<u64>,
    ) -> Result<Slot, Error> {
        let admission = &self.pool.admission;
        let mut state = admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Admission::check_open(&state, &self.pool)?;
        admission.check_memory_admission(&state, memory_bytes, priority)?;
        admission.drive(&mut state);
        admission.check_memory(&state, memory_bytes, priority)?;
        if !admission.available(&state, &self.pool, priority, memory_bytes) {
            return Err(Error::new(
                429,
                "request_slots",
                "No declared physical execution slot is available.",
            ));
        }
        let memory = admission.reserve(&mut state, &self.pool, priority, memory_bytes)?;
        Ok(Slot {
            pool: self.pool.clone(),
            priority,
            memory,
        })
    }

    pub async fn acquire(&self, priority: Priority) -> Result<Slot, Error> {
        self.acquire_inner(priority, None).await
    }

    pub async fn acquire_with_memory(&self, priority: Priority, bytes: u64) -> Result<Slot, Error> {
        self.acquire_inner(priority, Some(bytes)).await
    }

    async fn acquire_inner(
        &self,
        priority: Priority,
        memory_bytes: Option<u64>,
    ) -> Result<Slot, Error> {
        let admission = &self.pool.admission;
        let deadline = Instant::now() + admission.settings.wait(priority);
        let waiter = {
            let mut state = admission
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            Admission::check_open(&state, &self.pool)?;
            admission.check_memory_admission(&state, memory_bytes, priority)?;
            admission.drive(&mut state);
            if admission.available(&state, &self.pool, priority, memory_bytes) {
                let memory = admission.reserve(&mut state, &self.pool, priority, memory_bytes)?;
                return Ok(Slot {
                    pool: self.pool.clone(),
                    priority,
                    memory,
                });
            }
            if state.pending.len() >= admission.settings.pending_limit {
                return Err(Error::new(
                    429,
                    "pending_limit",
                    "The service-wide pending queue is full.",
                ));
            }
            if state
                .pending
                .iter()
                .filter(|waiter| Arc::ptr_eq(&waiter.pool, &self.pool))
                .count()
                >= admission.settings.service_pending_limit
            {
                return Err(Error::new(
                    429,
                    "service_pending_limit",
                    "The selected service's pending queue is full.",
                ));
            }
            let waiter = Arc::new(Waiter {
                pool: self.pool.clone(),
                priority,
                memory_bytes,
                deadline,
                status: AtomicU8::new(WAITING),
                changed: Notify::new(),
            });
            state.pending.push_back(waiter.clone());
            waiter
        };
        let mut waiting = Waiting {
            waiter,
            armed: true,
        };
        loop {
            match waiting.waiter.status.load(Ordering::Acquire) {
                GRANTED => {
                    if self.pool.closed.load(Ordering::Acquire) {
                        return Err(Admission::status_error(CLOSED));
                    }
                    waiting.armed = false;
                    return Ok(Slot {
                        pool: self.pool.clone(),
                        priority,
                        memory: admission.granted_memory(memory_bytes),
                    });
                }
                WAITING => {}
                status => return Err(Admission::status_error(status)),
            }
            tokio::select! {
                _ = waiting.waiter.changed.notified() => {},
                _ = tokio::time::sleep_until(deadline) => {
                    let mut state = admission.state.lock().unwrap_or_else(|error| error.into_inner());
                    admission.drive(&mut state);
                }
            }
        }
    }

    pub fn close(&self) {
        let admission = &self.pool.admission;
        let mut state = admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.pool.closed.store(true, Ordering::Relaxed);
        Admission::reject_waiters(&mut state, CLOSED, |waiter| {
            Arc::ptr_eq(&waiter.pool, &self.pool)
        });
        admission.drive(&mut state);
    }

    pub fn snapshot(&self) -> PoolSnapshot {
        let admission = &self.pool.admission;
        let state = admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        admission.pool_snapshot(&state, &self.pool)
    }
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let admission = &self.waiter.pool.admission;
        let mut state = admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        match self.waiter.status.load(Ordering::Relaxed) {
            WAITING => {
                state
                    .pending
                    .retain(|waiter| !Arc::ptr_eq(waiter, &self.waiter));
            }
            GRANTED => Admission::release(
                &mut state,
                &self.waiter.pool,
                self.waiter.priority,
                admission.granted_memory(self.waiter.memory_bytes),
            ),
            _ => return,
        }
        self.waiter.status.store(CANCELLED, Ordering::Release);
        admission.drive(&mut state);
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let admission = &self.pool.admission;
        let mut state = admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        Admission::release(&mut state, &self.pool, self.priority, self.memory.take());
        admission.drive(&mut state);
    }
}

fn memory_profile_required() -> Error {
    Error::new(
        503,
        "resource_profile_required",
        "Protected execution requires a server-bound resource profile and explicit host memory ceiling.",
    )
}

impl MemoryReservation {
    pub fn shrink_outstanding(&mut self, bytes: u64) -> Result<(), Error> {
        let Some(allocation) = self.allocation.as_mut() else {
            return Ok(());
        };
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.memory.shrink_outstanding(allocation, bytes)?;
        self.admission.drive(&mut state);
        Ok(())
    }

    pub fn materialize(&mut self, resident_bytes: u64) -> Result<(), Error> {
        let Some(allocation) = self.allocation.as_mut() else {
            return Ok(());
        };
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.memory.materialize(allocation, resident_bytes)?;
        self.admission.drive(&mut state);
        Ok(())
    }
}

impl Drop for MemoryReservation {
    fn drop(&mut self) {
        let Some(allocation) = self.allocation.take() else {
            return;
        };
        let mut state = self
            .admission
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.memory.release(allocation);
        self.admission.drive(&mut state);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;

    #[test]
    fn control_claims_bypass_pause_but_share_capacity_and_shutdown() {
        let admission = Admission::new(Settings {
            memory: MemorySettings {
                mode: MemoryMode::Protected,
                capacity_bytes: Some(100),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        admission.seed_host_sample_for_test(Some(100));
        let workload = admission.reserve_host(20, Priority::Interactive).unwrap();
        admission.pause(true);
        assert_eq!(
            admission
                .reserve_host(1, Priority::Interactive)
                .err()
                .unwrap()
                .code,
            "admission_paused"
        );
        let control = admission
            .reserve_control_host(30, Priority::Interactive)
            .unwrap();
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 50);
        assert!(
            admission
                .reserve_control_host(51, Priority::Interactive)
                .is_err()
        );
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 50);
        admission.shutdown();
        assert_eq!(
            admission
                .reserve_control_host(1, Priority::Interactive)
                .err()
                .unwrap()
                .code,
            "shutting_down"
        );
        drop(workload);
        drop(control);
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 0);
    }

    #[test]
    fn shrinking_outstanding_claims_preserves_growth_headroom_and_refuses_resident_or_zero_reductions()
     {
        let admission = Admission::new(Settings {
            memory: MemorySettings {
                mode: MemoryMode::Protected,
                capacity_bytes: Some(100),
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        admission.seed_host_sample_for_test(Some(100));
        let mut first = admission.reserve_host(80, Priority::Interactive).unwrap();
        assert!(admission.reserve_host(30, Priority::Interactive).is_err());
        first.shrink_outstanding(40).unwrap();
        assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 40);
        assert_eq!(admission.memory_snapshot().unwrap().resident_bytes, 0);
        let mut second = admission.reserve_host(60, Priority::Interactive).unwrap();
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 100);
        assert_eq!(
            second.shrink_outstanding(0).unwrap_err().code,
            "invalid_outstanding_reduction"
        );
        assert_eq!(
            second.shrink_outstanding(61).unwrap_err().code,
            "invalid_outstanding_reduction"
        );
        first.materialize(20).unwrap();
        assert_eq!(
            first.shrink_outstanding(10).unwrap_err().code,
            "invalid_outstanding_reduction"
        );
        assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 60);
        assert_eq!(admission.memory_snapshot().unwrap().resident_bytes, 20);
        drop(first);
        drop(second);
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 0);
    }

    fn setup(
        settings: Settings,
        capacity: Option<u32>,
        headroom: u32,
    ) -> (Arc<Admission>, SlotPool) {
        let admission = Admission::new(settings).unwrap();
        let pool = admission.pool("model", capacity, headroom).unwrap();
        (admission, pool)
    }
    fn pending<F: std::future::Future>(future: std::pin::Pin<&mut F>) {
        assert!(future.now_or_never().is_none());
    }

    #[tokio::test]
    async fn idle_pool_and_dropped_claims_do_not_consume_execution_capacity() {
        let (admission, pool) = setup(Settings::default(), Some(2), 0);
        let idle_client = pool.clone();
        assert_eq!(admission.snapshot().active, 0);
        let one = pool.acquire(Priority::Interactive).await.unwrap();
        let two = idle_client.acquire(Priority::Batch).await.unwrap();
        assert_eq!(pool.snapshot().active, 2);
        assert_eq!(
            pool.try_acquire(Priority::Interactive).err().unwrap().code,
            "request_slots"
        );
        drop(one);
        assert_eq!(admission.snapshot().active, 1);
        drop(two);
        assert_eq!(pool.snapshot().active, 0);
    }

    #[tokio::test]
    async fn waiting_and_granted_cancellation_release_exact_claims() {
        let (admission, pool) = setup(Settings::default(), Some(1), 0);
        let active = pool.acquire(Priority::Interactive).await.unwrap();
        let mut cancelled = Box::pin(pool.acquire(Priority::Interactive));
        pending(cancelled.as_mut());
        assert_eq!(admission.snapshot().pending, 1);
        drop(cancelled);
        assert_eq!(admission.snapshot().pending, 0);
        let mut granted = Box::pin(pool.acquire(Priority::Interactive));
        pending(granted.as_mut());
        drop(active);
        assert_eq!(admission.snapshot().active, 1);
        assert_eq!(admission.snapshot().pending, 0);
        drop(granted);
        assert_eq!(admission.snapshot().active, 0);
        assert!(pool.try_acquire(Priority::Interactive).is_ok());
    }

    #[tokio::test]
    async fn fifo_and_essential_bursts_give_oldest_ordinary_work_a_turn() {
        let (admission, pool) = setup(Settings::default(), Some(1), 0);
        let active = pool.acquire(Priority::Interactive).await.unwrap();
        let mut oldest_batch = Box::pin(pool.acquire(Priority::Batch));
        pending(oldest_batch.as_mut());
        let mut younger_interactive = Box::pin(pool.acquire(Priority::Interactive));
        pending(younger_interactive.as_mut());
        let mut essentials: Vec<_> = (0..9)
            .map(|_| Box::pin(pool.acquire(Priority::Essential)))
            .collect();
        for essential in &mut essentials {
            pending(essential.as_mut());
        }
        drop(active);
        for essential in essentials.iter_mut().take(8) {
            let slot = essential
                .as_mut()
                .now_or_never()
                .expect("essential grant")
                .unwrap();
            pending(oldest_batch.as_mut());
            drop(slot);
        }
        assert_eq!(admission.snapshot().essential_streak, 0);
        pending(essentials[8].as_mut());
        let ordinary = oldest_batch
            .as_mut()
            .now_or_never()
            .expect("ordinary grant after eight essentials")
            .unwrap();
        pending(younger_interactive.as_mut());
        drop(ordinary);
        let essential = essentials[8].as_mut().now_or_never().unwrap().unwrap();
        drop(essential);
        let interactive = younger_interactive
            .as_mut()
            .now_or_never()
            .unwrap()
            .unwrap();
        drop(interactive);
        assert_eq!(admission.snapshot().active, 0);
    }

    #[tokio::test]
    async fn blocked_service_does_not_reserve_global_capacity_or_block_other_services() {
        let admission = Admission::new(Settings {
            active_limit: Some(2),
            ..Settings::default()
        })
        .unwrap();
        let first = admission.pool("first", Some(1), 0).unwrap();
        let second = admission.pool("second", Some(1), 0).unwrap();
        let active = first.acquire(Priority::Interactive).await.unwrap();
        let mut blocked = Box::pin(first.acquire(Priority::Essential));
        pending(blocked.as_mut());
        assert_eq!(admission.snapshot().active, 1);
        let independent = second.acquire(Priority::Interactive).await.unwrap();
        assert_eq!(admission.snapshot().active, 2);
        drop(independent);
        assert_eq!(admission.snapshot().pending, 1);
        drop(active);
        drop(blocked.await.unwrap());
        assert_eq!(admission.snapshot().active, 0);
    }

    #[tokio::test]
    async fn queue_limits_reject_without_creating_additional_waiters() {
        let settings = Settings {
            pending_limit: 2,
            service_pending_limit: 1,
            ..Settings::default()
        };
        let (admission, first) = setup(settings, Some(1), 0);
        let second = admission.pool("second", Some(1), 0).unwrap();
        let first_active = first.acquire(Priority::Interactive).await.unwrap();
        let second_active = second.acquire(Priority::Interactive).await.unwrap();
        let mut first_wait = Box::pin(first.acquire(Priority::Interactive));
        pending(first_wait.as_mut());
        assert_eq!(
            first
                .acquire(Priority::Interactive)
                .await
                .err()
                .unwrap()
                .code,
            "service_pending_limit"
        );
        let mut second_wait = Box::pin(second.acquire(Priority::Interactive));
        pending(second_wait.as_mut());
        assert_eq!(
            first
                .acquire(Priority::Interactive)
                .await
                .err()
                .unwrap()
                .code,
            "pending_limit"
        );
        assert_eq!(admission.snapshot().pending, 2);
        drop(first_wait);
        drop(second_wait);
        drop(first_active);
        drop(second_active);
        assert_eq!(admission.snapshot().active, 0);
        assert_eq!(admission.snapshot().pending, 0);
    }

    #[tokio::test]
    async fn deadlines_expire_pending_work_without_releasing_active_requests() {
        let (admission, pool) = setup(
            Settings {
                interactive_wait_ms: 10,
                ..Settings::default()
            },
            Some(1),
            0,
        );
        let active = pool.acquire(Priority::Interactive).await.unwrap();
        let error =
            tokio::time::timeout(Duration::from_secs(1), pool.acquire(Priority::Interactive))
                .await
                .unwrap()
                .err()
                .unwrap();
        assert_eq!(error.code, "queue_timeout");
        assert_eq!(admission.snapshot().pending, 0);
        assert_eq!(admission.snapshot().active, 1);
        drop(active);
    }

    #[tokio::test]
    async fn pause_shutdown_and_pool_retirement_cancel_waiters_but_keep_active_claims() {
        let (admission, pool) = setup(Settings::default(), Some(1), 0);
        let active = pool.acquire(Priority::Interactive).await.unwrap();
        let mut waiting = Box::pin(pool.acquire(Priority::Interactive));
        pending(waiting.as_mut());
        admission.pause(true);
        assert_eq!(waiting.await.err().unwrap().code, "admission_paused");
        assert_eq!(
            pool.acquire(Priority::Interactive)
                .await
                .err()
                .unwrap()
                .code,
            "admission_paused"
        );
        assert_eq!(admission.snapshot().active, 1);
        admission.pause(false);
        let mut waiting = Box::pin(pool.acquire(Priority::Interactive));
        pending(waiting.as_mut());
        pool.close();
        assert_eq!(waiting.await.err().unwrap().code, "worker_closed");
        assert_eq!(
            pool.acquire(Priority::Essential).await.err().unwrap().code,
            "worker_closed"
        );
        assert!(admission.pool("model", Some(1), 0).is_err());
        drop(active);
        let replacement = admission.pool("model", Some(1), 0).unwrap();
        let active = replacement.acquire(Priority::Interactive).await.unwrap();
        let mut waiting = Box::pin(replacement.acquire(Priority::Interactive));
        pending(waiting.as_mut());
        admission.shutdown();
        admission.pause(false);
        assert_eq!(waiting.await.err().unwrap().code, "shutting_down");
        assert_eq!(
            replacement
                .acquire(Priority::Interactive)
                .await
                .err()
                .unwrap()
                .code,
            "shutting_down"
        );
        assert_eq!(admission.snapshot().active, 1);
        drop(active);
        assert_eq!(admission.snapshot().active, 0);
    }

    #[tokio::test]
    async fn retiring_a_granted_but_unpolled_waiter_releases_its_claim() {
        let (admission, pool) = setup(Settings::default(), Some(1), 0);
        let active = pool.acquire(Priority::Interactive).await.unwrap();
        let mut waiting = Box::pin(pool.acquire(Priority::Interactive));
        pending(waiting.as_mut());
        drop(active);
        assert_eq!(admission.snapshot().active, 1);
        pool.close();
        assert_eq!(waiting.await.err().unwrap().code, "worker_closed");
        assert_eq!(admission.snapshot().active, 0);
    }

    #[tokio::test]
    async fn headroom_is_reserved_for_essential_work_and_capacity_is_never_invented() {
        let (admission, pool) = setup(Settings::default(), Some(2), 1);
        let ordinary = pool.acquire(Priority::Interactive).await.unwrap();
        let mut waiting = Box::pin(pool.acquire(Priority::Batch));
        pending(waiting.as_mut());
        let essential = pool.acquire(Priority::Essential).await.unwrap();
        assert_eq!(admission.snapshot().active, 2);
        drop(ordinary);
        let second_ordinary = waiting.await.unwrap();
        assert_eq!(pool.snapshot().ordinary_active, 1);
        drop(second_ordinary);
        drop(essential);
        let unknown = admission.pool("unknown", None, 0).unwrap();
        assert_eq!(
            unknown
                .acquire(Priority::Essential)
                .await
                .err()
                .unwrap()
                .code,
            "unknown_slots"
        );
        assert_eq!(admission.snapshot().active, 0);
    }

    #[tokio::test]
    async fn an_expired_unpolled_waiter_cannot_claim_a_released_slot() {
        let (admission, pool) = setup(
            Settings {
                interactive_wait_ms: 5,
                ..Settings::default()
            },
            Some(1),
            0,
        );
        let active = pool.acquire(Priority::Interactive).await.unwrap();
        let mut waiting = Box::pin(pool.acquire(Priority::Interactive));
        pending(waiting.as_mut());
        tokio::time::sleep(Duration::from_millis(10)).await;
        drop(active);
        assert_eq!(admission.snapshot().active, 0);
        assert_eq!(admission.snapshot().pending, 0);
        assert_eq!(waiting.await.err().unwrap().code, "queue_timeout");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_workers_never_exceed_pool_or_shared_limits() {
        let admission = Admission::new(Settings {
            active_limit: Some(4),
            service_pending_limit: 64,
            ..Settings::default()
        })
        .unwrap();
        let first = admission.pool("first", Some(3), 0).unwrap();
        let second = admission.pool("second", Some(2), 0).unwrap();
        let mut tasks = Vec::new();
        for index in 0..64 {
            let admission = admission.clone();
            let pool = if index % 2 == 0 {
                first.clone()
            } else {
                second.clone()
            };
            tasks.push(tokio::spawn(async move {
                let slot = pool.acquire(Priority::Interactive).await.unwrap();
                let view = admission.snapshot();
                assert!(view.active <= 4);
                assert!(
                    view.pools
                        .iter()
                        .all(|pool| pool.active <= pool.physical_capacity.unwrap())
                );
                assert_eq!(
                    view.active,
                    view.pools
                        .iter()
                        .map(|pool| pool.active as usize)
                        .sum::<usize>()
                );
                tokio::task::yield_now().await;
                drop(slot);
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(admission.snapshot().active, 0);
        assert_eq!(admission.snapshot().pending, 0);
    }

    #[test]
    fn configuration_defaults_preserve_optional_limits_and_reject_invalid_values() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.pending_limit, 64);
        assert_eq!(settings.service_pending_limit, 16);
        assert_eq!(settings.active_limit, None);
        assert_eq!(
            (
                settings.essential_wait_ms,
                settings.interactive_wait_ms,
                settings.batch_wait_ms
            ),
            (2000, 10000, 30000)
        );
        assert!(serde_json::from_str::<Settings>(r#"{"principal_pending_limit":1}"#).is_err());
        for value in [
            r#"{"active_limit":0}"#,
            r#"{"pending_limit":4097}"#,
            r#"{"batch_wait_ms":300001}"#,
        ] {
            let settings: Settings = serde_json::from_str(value).unwrap();
            assert!(settings.validate().is_err());
        }
    }
    #[test]
    fn ingress_settings_default_to_a_shared_bounded_byte_budget() {
        let settings: Settings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings.ingress_body_bytes, 128 * 1024 * 1024);
        assert!(settings.validate().is_ok());
        assert!(validate_ingress_body_bytes(MIN_INGRESS_BODY_BYTES).is_ok());
        assert!(validate_ingress_body_bytes(MAX_INGRESS_BODY_BYTES).is_ok());
        for bytes in [
            0,
            MIN_INGRESS_BODY_BYTES - 1,
            MAX_INGRESS_BODY_BYTES + 1,
            usize::MAX,
        ] {
            assert_eq!(
                validate_ingress_body_bytes(bytes).unwrap_err().code,
                "invalid_ingress_capacity"
            );
            assert!(
                Settings {
                    ingress_body_bytes: bytes,
                    ..Settings::default()
                }
                .validate()
                .is_err()
            );
        }
        assert!(serde_json::from_str::<Settings>(r#"{"ingress_body_bytes":-1}"#).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"ingress_body_bytes":1.5}"#).is_err());
    }

    #[test]
    fn ingress_exhaustion_and_invalid_claims_leave_exact_accounting() {
        let ingress = Ingress::new(MIN_INGRESS_BODY_BYTES);
        for bytes in [0, MIN_INGRESS_BODY_BYTES + 1, usize::MAX] {
            let error = ingress.try_reserve(bytes).err().unwrap();
            assert_eq!((error.status, error.code), (503, "ingress_capacity"));
            assert_eq!(ingress.view().reserved_bytes, 0);
        }
        let json = ingress.try_reserve(JSON_INGRESS_BYTES).unwrap();
        assert_eq!(
            ingress.view().available_bytes,
            SPEECH_INGRESS_BYTES - JSON_INGRESS_BYTES
        );
        assert!(ingress.try_reserve(SPEECH_INGRESS_BYTES).is_err());
        assert_eq!(ingress.view().reserved_bytes, JSON_INGRESS_BYTES);
        drop(json);
        let speech = ingress.try_reserve(SPEECH_INGRESS_BYTES).unwrap();
        assert_eq!(ingress.view().available_bytes, 0);
        assert!(ingress.try_reserve(1).is_err());
        drop(speech);
        assert_eq!(
            ingress.view(),
            IngressSnapshot {
                capacity_bytes: SPEECH_INGRESS_BYTES,
                reserved_bytes: 0,
                available_bytes: SPEECH_INGRESS_BYTES
            }
        );
        assert!(Ingress::new(0).try_reserve(1).is_err());
    }

    #[tokio::test]
    async fn cancelling_a_handler_releases_its_ingress_claim() {
        let ingress = Ingress::new(SPEECH_INGRESS_BYTES);
        let started = Arc::new(Notify::new());
        let handler_ingress = ingress.clone();
        let handler_started = started.clone();
        let handler = tokio::spawn(async move {
            let _lease = handler_ingress.try_reserve(SPEECH_INGRESS_BYTES).unwrap();
            handler_started.notify_one();
            std::future::pending::<()>().await;
        });
        started.notified().await;
        assert_eq!(ingress.view().reserved_bytes, SPEECH_INGRESS_BYTES);
        handler.abort();
        assert!(handler.await.unwrap_err().is_cancelled());
        assert_eq!(ingress.view().reserved_bytes, 0);
        assert!(ingress.try_reserve(SPEECH_INGRESS_BYTES).is_ok());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn mixed_concurrent_ingress_claims_share_one_capacity_without_a_wait_queue() {
        let ingress = Ingress::new(default_ingress_body_bytes());
        let acquired = Arc::new(tokio::sync::Barrier::new(65));
        let release = Arc::new(tokio::sync::Barrier::new(65));
        let accepted_bytes = Arc::new(AtomicUsize::new(0));
        let refusals = Arc::new(AtomicUsize::new(0));
        let mut handlers = Vec::new();
        for index in 0..64 {
            let ingress = ingress.clone();
            let acquired = acquired.clone();
            let release = release.clone();
            let accepted_bytes = accepted_bytes.clone();
            let refusals = refusals.clone();
            handlers.push(tokio::spawn(async move {
                let bytes = if index % 2 == 0 {
                    JSON_INGRESS_BYTES
                } else {
                    SPEECH_INGRESS_BYTES
                };
                let lease = match ingress.try_reserve(bytes) {
                    Ok(lease) => {
                        accepted_bytes.fetch_add(bytes, Ordering::SeqCst);
                        Some(lease)
                    }
                    Err(error) => {
                        assert_eq!(error.code, "ingress_capacity");
                        refusals.fetch_add(1, Ordering::SeqCst);
                        None
                    }
                };
                acquired.wait().await;
                release.wait().await;
                drop(lease);
            }));
        }
        acquired.wait().await;
        let view = ingress.view();
        assert_eq!(view.reserved_bytes, accepted_bytes.load(Ordering::SeqCst));
        assert!(view.reserved_bytes <= view.capacity_bytes);
        assert_eq!(
            view.reserved_bytes + view.available_bytes,
            view.capacity_bytes
        );
        assert!(refusals.load(Ordering::SeqCst) > 0);
        assert!(ingress.try_reserve(SPEECH_INGRESS_BYTES).is_err());
        release.wait().await;
        for handler in handlers {
            handler.await.unwrap();
        }
        assert_eq!(ingress.view().reserved_bytes, 0);
        assert_eq!(ingress.view().available_bytes, default_ingress_body_bytes());
    }
}
