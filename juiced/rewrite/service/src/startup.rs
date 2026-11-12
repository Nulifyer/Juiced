use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};
use tokio::{sync::Notify, time::Instant};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub concurrent: usize,
    pub pending: usize,
    pub wait_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            concurrent: 1,
            pending: 64,
            wait_ms: 30_000,
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), Error> {
        if !(1..=64).contains(&self.concurrent)
            || self.pending > 4096
            || !(1..=300_000).contains(&self.wait_ms)
        {
            Err(Error::new(
                400,
                "invalid_startup_settings",
                "Startup concurrency must be 1 to 64, pending capacity 0 to 4096, and queue wait 1 to 300000 milliseconds.",
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
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

const WAITING: u8 = 0;
const GRANTED: u8 = 1;
const EXPIRED: u8 = 2;
const PAUSED: u8 = 3;
const STOPPED: u8 = 4;
const CANCELLED: u8 = 5;
const SNAPSHOT_MODELS: usize = 64;

fn failure(status: u8) -> Error {
    match status {
        EXPIRED => Error::new(
            504,
            "startup_queue_timeout",
            "The model could not enter startup before its queue deadline.",
        ),
        PAUSED => Error::new(503, "startup_paused", "New model startups are paused."),
        STOPPED => Error::new(503, "shutting_down", "The service is shutting down."),
        _ => Error::new(
            503,
            "startup_unavailable",
            "The startup request is no longer available.",
        ),
    }
}

struct Waiter {
    id: u64,
    model: Arc<str>,
    deadline: Instant,
    epoch: u64,
    status: AtomicU8,
    changed: Notify,
}

struct State {
    next: u64,
    active: BTreeMap<u64, Claim>,
    pending: VecDeque<Arc<Waiter>>,
    paused: bool,
    shutting_down: bool,
    control_epoch: u64,
    last_interruption: Option<Error>,
}

enum Claim {
    Owned(Arc<str>),
    Unclaimed(Arc<Waiter>),
}

impl Claim {
    fn model(&self) -> &Arc<str> {
        match self {
            Self::Owned(model) => model,
            Self::Unclaimed(waiter) => &waiter.model,
        }
    }
}

pub struct StartupGate {
    settings: Settings,
    state: Mutex<State>,
    interrupted: Notify,
}

pub struct Interruption {
    gate: Arc<StartupGate>,
    epoch: u64,
    immediate: Option<Error>,
}

impl Interruption {
    pub async fn wait(&self) -> Error {
        loop {
            let notified = self.gate.interrupted.notified();
            {
                let state = self.gate.lock();
                if state.shutting_down {
                    return failure(STOPPED);
                }
                if let Some(error) = self.immediate {
                    return error;
                }
                if state.control_epoch != self.epoch {
                    return state.last_interruption.unwrap_or_else(|| failure(STOPPED));
                }
            }
            notified.await;
        }
    }
}

#[must_use = "The permit must remain owned until startup and its retained cleanup settle"]
pub struct StartupPermit {
    gate: Arc<StartupGate>,
    id: u64,
}

impl fmt::Debug for StartupPermit {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StartupPermit")
            .finish_non_exhaustive()
    }
}

struct Registration {
    gate: Arc<StartupGate>,
    waiter: Arc<Waiter>,
    armed: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub settings: Settings,
    pub active: usize,
    pub pending: usize,
    pub paused: bool,
    pub shutting_down: bool,
    pub active_models: Vec<Arc<str>>,
    pub pending_models: Vec<Arc<str>>,
    pub models_truncated: bool,
}

impl StartupGate {
    pub fn new(settings: Settings) -> Result<Arc<Self>, Error> {
        settings.validate()?;
        Ok(Arc::new(Self {
            settings,
            state: Mutex::new(State {
                next: 1,
                active: BTreeMap::new(),
                pending: VecDeque::new(),
                paused: false,
                shutting_down: false,
                control_epoch: 0,
                last_interruption: None,
            }),
            interrupted: Notify::new(),
        }))
    }

    pub async fn acquire(
        self: &Arc<Self>,
        model: Arc<str>,
        deadline: Instant,
    ) -> Result<StartupPermit, Error> {
        if model.len() > 256 || model.trim().is_empty() || model.chars().any(char::is_control) {
            return Err(Error::new(
                400,
                "invalid_startup_model",
                "Startup requires a model ID of at most 256 bytes without control characters.",
            ));
        }
        let deadline = deadline.min(Instant::now() + Duration::from_millis(self.settings.wait_ms));
        let waiter = {
            let mut state = self.lock();
            if state.shutting_down {
                return Err(failure(STOPPED));
            }
            if state.paused {
                return Err(failure(PAUSED));
            }
            if Instant::now() >= deadline {
                return Err(failure(EXPIRED));
            }
            let wake = self.dispatch(&mut state);
            if state.active.len() >= self.settings.concurrent
                && state.pending.len() >= self.settings.pending
            {
                drop(state);
                notify(wake);
                return Err(Error::new(
                    429,
                    "startup_queue_full",
                    "The startup queue is full. Retry after another model finishes starting.",
                ));
            }
            let id = state.next;
            let Some(next) = id.checked_add(1) else {
                drop(state);
                notify(wake);
                return Err(Error::new(
                    503,
                    "startup_sequence_limit",
                    "The startup gate cannot accept another request.",
                ));
            };
            state.next = next;
            if state.pending.is_empty() && state.active.len() < self.settings.concurrent {
                state.active.insert(id, Claim::Owned(model));
                drop(state);
                let permit = StartupPermit {
                    gate: self.clone(),
                    id,
                };
                notify(wake);
                if Instant::now() >= deadline {
                    return Err(failure(EXPIRED));
                }
                return Ok(permit);
            }
            let waiter = Arc::new(Waiter {
                id,
                model,
                deadline,
                epoch: state.control_epoch,
                status: AtomicU8::new(WAITING),
                changed: Notify::new(),
            });
            state.pending.push_back(waiter.clone());
            drop(state);
            notify(wake);
            waiter
        };
        let mut registration = Registration {
            gate: self.clone(),
            waiter,
            armed: true,
        };
        loop {
            let notified = registration.waiter.changed.notified();
            match registration.waiter.status.load(Ordering::Acquire) {
                GRANTED => {
                    {
                        let mut state = self.lock();
                        if state.shutting_down {
                            return Err(failure(STOPPED));
                        }
                        if state.paused {
                            return Err(failure(PAUSED));
                        }
                        if state.control_epoch != registration.waiter.epoch {
                            return Err(state
                                .last_interruption
                                .unwrap_or_else(|| failure(STOPPED)));
                        }
                        if Instant::now() >= deadline {
                            return Err(failure(EXPIRED));
                        }
                        if !state.active.contains_key(&registration.waiter.id) {
                            return Err(failure(CANCELLED));
                        }
                        state.active.insert(
                            registration.waiter.id,
                            Claim::Owned(registration.waiter.model.clone()),
                        );
                    }
                    registration.armed = false;
                    return Ok(StartupPermit {
                        gate: self.clone(),
                        id: registration.waiter.id,
                    });
                }
                WAITING => {
                    if Instant::now() >= deadline {
                        return Err(failure(EXPIRED));
                    }
                    let _ = tokio::time::timeout_at(deadline, notified).await;
                }
                status => return Err(failure(status)),
            }
        }
    }

    pub fn pause(&self, paused: bool) {
        let mut state = self.lock();
        let interrupt = paused && !state.paused && !state.shutting_down;
        state.paused = paused;
        if interrupt {
            advance_control(&mut state, failure(PAUSED));
        }
        let mut wake = if paused {
            reject(&mut state, PAUSED)
        } else {
            Vec::new()
        };
        wake.extend(self.dispatch(&mut state));
        drop(state);
        notify(wake);
        if interrupt {
            self.interrupted.notify_waiters();
        }
    }

    pub fn shutdown(&self) {
        let mut state = self.lock();
        if !state.shutting_down {
            advance_control(&mut state, failure(STOPPED));
        }
        state.shutting_down = true;
        let mut wake = reject(&mut state, STOPPED);
        wake.extend(self.dispatch(&mut state));
        drop(state);
        notify(wake);
        self.interrupted.notify_waiters();
    }

    pub fn interruption(self: &Arc<Self>) -> Interruption {
        let state = self.lock();
        Interruption {
            gate: self.clone(),
            epoch: state.control_epoch,
            immediate: if state.shutting_down {
                Some(failure(STOPPED))
            } else if state.paused {
                Some(failure(PAUSED))
            } else {
                None
            },
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        Snapshot {
            settings: self.settings.clone(),
            active: state.active.len(),
            pending: state.pending.len(),
            paused: state.paused,
            shutting_down: state.shutting_down,
            active_models: state
                .active
                .values()
                .take(SNAPSHOT_MODELS)
                .map(|claim| claim.model().clone())
                .collect(),
            pending_models: state
                .pending
                .iter()
                .take(SNAPSHOT_MODELS)
                .map(|entry| entry.model.clone())
                .collect(),
            models_truncated: state.active.len() > SNAPSHOT_MODELS
                || state.pending.len() > SNAPSHOT_MODELS,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn dispatch(&self, state: &mut State) -> Vec<Arc<Waiter>> {
        let mut wake = Vec::new();
        let now = Instant::now();
        state.active.retain(|_, claim| {
            if let Claim::Unclaimed(entry) = claim {
                let status = if state.shutting_down {
                    Some(STOPPED)
                } else if entry.epoch != state.control_epoch {
                    Some(PAUSED)
                } else if entry.deadline <= now {
                    Some(EXPIRED)
                } else {
                    None
                };
                if let Some(status) = status {
                    entry.status.store(status, Ordering::Release);
                    wake.push(entry.clone());
                    return false;
                }
            }
            true
        });
        state.pending.retain(|entry| {
            if entry.deadline <= now {
                entry.status.store(EXPIRED, Ordering::Release);
                wake.push(entry.clone());
                false
            } else {
                true
            }
        });
        while !state.paused && !state.shutting_down && state.active.len() < self.settings.concurrent
        {
            let Some(entry) = state.pending.pop_front() else {
                break;
            };
            state
                .active
                .insert(entry.id, Claim::Unclaimed(entry.clone()));
            entry.status.store(GRANTED, Ordering::Release);
            wake.push(entry);
        }
        wake
    }

    fn release(&self, id: u64, cancelled: bool) {
        let mut state = self.lock();
        if cancelled
            && let Some(position) = state.pending.iter().position(|entry| entry.id == id)
            && let Some(entry) = state.pending.remove(position)
        {
            entry.status.store(CANCELLED, Ordering::Release);
        }
        state.active.remove(&id);
        let wake = self.dispatch(&mut state);
        drop(state);
        notify(wake);
    }
}

fn advance_control(state: &mut State, error: Error) {
    if let Some(epoch) = state.control_epoch.checked_add(1) {
        state.control_epoch = epoch;
        state.last_interruption = Some(error);
    } else {
        state.shutting_down = true;
        state.last_interruption = Some(failure(STOPPED));
    }
}

fn reject(state: &mut State, status: u8) -> Vec<Arc<Waiter>> {
    let wake = state.pending.drain(..).collect::<Vec<_>>();
    for entry in &wake {
        entry.status.store(status, Ordering::Release);
    }
    wake
}

fn notify(waiters: Vec<Arc<Waiter>>) {
    for waiter in waiters {
        waiter.changed.notify_waiters();
    }
}

impl Drop for StartupPermit {
    fn drop(&mut self) {
        self.gate.release(self.id, false);
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if self.armed {
            self.gate.release(self.waiter.id, true);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }
    fn model(value: &str) -> Arc<str> {
        Arc::from(value)
    }
    fn poll<F: Future>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }
    fn ready<T>(value: Poll<T>) -> T {
        match value {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("the startup result must be ready"),
        }
    }
    fn gate() -> Arc<StartupGate> {
        StartupGate::new(Settings::default()).unwrap()
    }

    #[test]
    fn settings_default_and_validate_without_silent_capacity_changes() {
        assert_eq!(
            serde_json::from_str::<Settings>("{}").unwrap(),
            Settings::default()
        );
        assert_eq!(
            serde_json::from_str::<Settings>(r#"{"concurrent":2}"#).unwrap(),
            Settings {
                concurrent: 2,
                ..Default::default()
            }
        );
        for invalid in [
            r#"{"unknown":1}"#,
            r#"{"pending":-1}"#,
            r#"{"wait_ms":1.5}"#,
        ] {
            assert!(serde_json::from_str::<Settings>(invalid).is_err());
        }
        for settings in [
            Settings {
                concurrent: 0,
                ..Default::default()
            },
            Settings {
                concurrent: 65,
                ..Default::default()
            },
            Settings {
                pending: 4097,
                ..Default::default()
            },
            Settings {
                wait_ms: 0,
                ..Default::default()
            },
            Settings {
                wait_ms: 300001,
                ..Default::default()
            },
        ] {
            assert_eq!(
                settings.validate().unwrap_err().code,
                "invalid_startup_settings"
            );
            assert!(StartupGate::new(settings).is_err());
        }
        assert!(
            StartupGate::new(Settings {
                concurrent: 64,
                pending: 0,
                wait_ms: 1
            })
            .is_ok()
        );
    }

    #[tokio::test]
    async fn no_queue_mode_grants_only_available_capacity_and_reuses_released_permits() {
        let gate = StartupGate::new(Settings {
            concurrent: 2,
            pending: 0,
            ..Default::default()
        })
        .unwrap();
        let first = gate.acquire(model("first"), deadline()).await.unwrap();
        let second = gate.acquire(model("second"), deadline()).await.unwrap();
        let error = gate.acquire(model("third"), deadline()).await.unwrap_err();
        assert_eq!((error.status, error.code), (429, "startup_queue_full"));
        assert_eq!(gate.snapshot().active, 2);
        assert_eq!(gate.snapshot().pending, 0);
        drop(first);
        let third = gate.acquire(model("third"), deadline()).await.unwrap();
        assert_eq!(gate.snapshot().active, 2);
        drop(second);
        drop(third);
        assert_eq!(gate.snapshot().active, 0);
    }

    #[tokio::test]
    async fn queue_is_fifo_and_new_arrivals_cannot_bypass_an_unpolled_older_grant() {
        let gate = gate();
        let first = gate.acquire(model("first"), deadline()).await.unwrap();
        let mut second = Box::pin(gate.acquire(model("second"), deadline()));
        let mut third = Box::pin(gate.acquire(model("third"), deadline()));
        assert!(poll(second.as_mut()).is_pending());
        assert!(poll(third.as_mut()).is_pending());
        assert_eq!(
            gate.snapshot().pending_models,
            vec![model("second"), model("third")]
        );
        drop(first);
        let mut fourth = Box::pin(gate.acquire(model("fourth"), deadline()));
        assert!(poll(fourth.as_mut()).is_pending());
        assert_eq!(gate.snapshot().active_models, vec![model("second")]);
        let second = ready(poll(second.as_mut())).unwrap();
        drop(second);
        assert_eq!(gate.snapshot().active_models, vec![model("third")]);
        let third = ready(poll(third.as_mut())).unwrap();
        drop(third);
        let fourth = ready(poll(fourth.as_mut())).unwrap();
        drop(fourth);
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }

    #[tokio::test]
    async fn queue_limit_counts_waiters_and_cancellation_refunds_without_starting_work() {
        let gate = StartupGate::new(Settings {
            pending: 1,
            ..Default::default()
        })
        .unwrap();
        let first = gate.acquire(model("first"), deadline()).await.unwrap();
        let mut waiting = Box::pin(gate.acquire(model("waiting"), deadline()));
        assert!(poll(waiting.as_mut()).is_pending());
        assert_eq!(
            gate.acquire(model("overflow"), deadline())
                .await
                .unwrap_err()
                .code,
            "startup_queue_full"
        );
        drop(waiting);
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (1, 0));
        let mut next = Box::pin(gate.acquire(model("next"), deadline()));
        assert!(poll(next.as_mut()).is_pending());
        drop(first);
        drop(ready(poll(next.as_mut())).unwrap());
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }

    #[tokio::test]
    async fn configured_and_caller_deadlines_do_not_extend_each_other() {
        for (wait_ms, caller_wait) in [
            (20, Duration::from_secs(2)),
            (2000, Duration::from_millis(20)),
        ] {
            let gate = StartupGate::new(Settings {
                wait_ms,
                ..Default::default()
            })
            .unwrap();
            let held = gate.acquire(model("held"), deadline()).await.unwrap();
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                gate.acquire(model("waiting"), Instant::now() + caller_wait),
            )
            .await
            .unwrap();
            let error = result.unwrap_err();
            assert_eq!((error.status, error.code), (504, "startup_queue_timeout"));
            assert_eq!((gate.snapshot().active, gate.snapshot().pending), (1, 0));
            drop(held);
        }
        let gate = gate();
        assert_eq!(
            gate.acquire(model("already-expired"), Instant::now())
                .await
                .unwrap_err()
                .code,
            "startup_queue_timeout"
        );
        assert_eq!(gate.snapshot().active, 0);
    }

    #[tokio::test]
    async fn dropping_an_unpolled_grant_hands_capacity_to_the_next_waiter_exactly_once() {
        let gate = gate();
        let held = gate.acquire(model("held"), deadline()).await.unwrap();
        let mut abandoned = Box::pin(gate.acquire(model("abandoned"), deadline()));
        let mut next = Box::pin(gate.acquire(model("next"), deadline()));
        assert!(poll(abandoned.as_mut()).is_pending());
        assert!(poll(next.as_mut()).is_pending());
        drop(held);
        assert_eq!(gate.snapshot().active_models, vec![model("abandoned")]);
        drop(abandoned);
        assert_eq!(gate.snapshot().active_models, vec![model("next")]);
        drop(ready(poll(next.as_mut())).unwrap());
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }

    #[tokio::test]
    async fn expired_unpolled_grant_cannot_be_claimed_after_its_deadline() {
        let gate = gate();
        let held = gate.acquire(model("held"), deadline()).await.unwrap();
        let mut expired =
            Box::pin(gate.acquire(model("late"), Instant::now() + Duration::from_millis(20)));
        assert!(poll(expired.as_mut()).is_pending());
        drop(held);
        tokio::time::sleep(Duration::from_millis(35)).await;
        assert_eq!(
            ready(poll(expired.as_mut())).unwrap_err().code,
            "startup_queue_timeout"
        );
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }

    #[tokio::test]
    async fn owned_startup_keeps_capacity_after_its_acquisition_deadline_expires() {
        let gate = gate();
        let owned = gate
            .acquire(model("owned"), Instant::now() + Duration::from_millis(50))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(75)).await;
        let mut next = Box::pin(gate.acquire(model("next"), deadline()));
        assert!(poll(next.as_mut()).is_pending());
        assert_eq!(gate.snapshot().active_models, vec![model("owned")]);
        drop(owned);
        drop(ready(poll(next.as_mut())).unwrap());
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }

    #[tokio::test]
    async fn admission_reclaims_expired_unpolled_grants_before_their_caller_polls() {
        let gate = gate();
        let held = gate.acquire(model("held"), deadline()).await.unwrap();
        let mut expired =
            Box::pin(gate.acquire(model("expired"), Instant::now() + Duration::from_millis(20)));
        let mut next = Box::pin(gate.acquire(model("next"), deadline()));
        assert!(poll(expired.as_mut()).is_pending());
        assert!(poll(next.as_mut()).is_pending());
        drop(held);
        tokio::time::sleep(Duration::from_millis(35)).await;
        let mut newcomer = Box::pin(gate.acquire(model("newcomer"), deadline()));
        assert!(poll(newcomer.as_mut()).is_pending());
        assert_eq!(gate.snapshot().active_models, vec![model("next")]);
        assert_eq!(
            ready(poll(expired.as_mut())).unwrap_err().code,
            "startup_queue_timeout"
        );
        assert_eq!(gate.snapshot().active_models, vec![model("next")]);
        drop(ready(poll(next.as_mut())).unwrap());
        drop(ready(poll(newcomer.as_mut())).unwrap());
        assert_eq!(gate.snapshot().active, 0);
    }

    #[tokio::test]
    async fn expired_queue_tail_does_not_consume_capacity_or_reorder_surviving_waiters() {
        let gate = StartupGate::new(Settings {
            pending: 2,
            ..Default::default()
        })
        .unwrap();
        let held = gate.acquire(model("held"), deadline()).await.unwrap();
        let mut long = Box::pin(gate.acquire(model("long"), deadline()));
        let mut short =
            Box::pin(gate.acquire(model("short"), Instant::now() + Duration::from_millis(20)));
        assert!(poll(long.as_mut()).is_pending());
        assert!(poll(short.as_mut()).is_pending());
        tokio::time::sleep(Duration::from_millis(35)).await;
        let mut new = Box::pin(gate.acquire(model("new"), deadline()));
        assert!(poll(new.as_mut()).is_pending());
        assert_eq!(
            gate.snapshot().pending_models,
            vec![model("long"), model("new")]
        );
        assert_eq!(
            ready(poll(short.as_mut())).unwrap_err().code,
            "startup_queue_timeout"
        );
        drop(held);
        drop(ready(poll(long.as_mut())).unwrap());
        drop(ready(poll(new.as_mut())).unwrap());
        assert_eq!(gate.snapshot().active, 0);
    }

    #[tokio::test]
    async fn pause_resume_and_shutdown_reject_unclaimed_grants_but_keep_owned_permits() {
        for shutdown in [false, true] {
            let gate = gate();
            let held = gate.acquire(model("held"), deadline()).await.unwrap();
            let mut unclaimed = Box::pin(gate.acquire(model("unclaimed"), deadline()));
            assert!(poll(unclaimed.as_mut()).is_pending());
            drop(held);
            if shutdown {
                gate.shutdown();
            } else {
                gate.pause(true);
                gate.pause(false);
            }
            let expected = if shutdown {
                "shutting_down"
            } else {
                "startup_paused"
            };
            assert_eq!(ready(poll(unclaimed.as_mut())).unwrap_err().code, expected);
            assert_eq!(gate.snapshot().active, 0);
            if shutdown {
                gate.pause(false);
                assert_eq!(
                    gate.acquire(model("new"), deadline())
                        .await
                        .unwrap_err()
                        .code,
                    "shutting_down"
                );
            } else {
                let owned = gate.acquire(model("owned"), deadline()).await.unwrap();
                gate.pause(true);
                assert_eq!(gate.snapshot().active, 1);
                assert_eq!(
                    gate.acquire(model("new"), deadline())
                        .await
                        .unwrap_err()
                        .code,
                    "startup_paused"
                );
                drop(owned);
                assert_eq!(gate.snapshot().active, 0);
            }
        }
    }

    #[tokio::test]
    async fn shutdown_wakes_queued_waiters_and_preserves_active_until_drop() {
        let gate = gate();
        let held = gate.acquire(model("held"), deadline()).await.unwrap();
        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire(model("waiting"), deadline()).await })
        };
        let limit = deadline();
        while gate.snapshot().pending != 1 {
            assert!(
                Instant::now() < limit,
                "the waiter must register before the test deadline"
            );
            tokio::task::yield_now().await;
        }
        gate.shutdown();
        assert_eq!(waiting.await.unwrap().unwrap_err().code, "shutting_down");
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (1, 0));
        drop(held);
        assert_eq!(gate.snapshot().active, 0);
    }

    #[tokio::test]
    async fn release_wakes_an_async_waiter_without_waiting_for_its_timeout() {
        let gate = gate();
        let held = gate.acquire(model("held"), deadline()).await.unwrap();
        let waiting = {
            let gate = gate.clone();
            tokio::spawn(async move { gate.acquire(model("next"), deadline()).await })
        };
        let limit = deadline();
        while gate.snapshot().pending != 1 {
            assert!(
                Instant::now() < limit,
                "the waiter must register before the test deadline"
            );
            tokio::task::yield_now().await;
        }
        drop(held);
        let granted = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(gate.snapshot().active_models, vec![model("next")]);
        drop(granted);
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }

    #[tokio::test]
    async fn cancellation_of_an_owned_permit_returns_capacity_to_the_oldest_waiter() {
        let gate = gate();
        let (ready_send, ready_receive) = tokio::sync::oneshot::channel();
        let active = {
            let gate = gate.clone();
            tokio::spawn(async move {
                let _permit = gate.acquire(model("owned"), deadline()).await.unwrap();
                ready_send.send(()).unwrap();
                std::future::pending::<()>().await;
            })
        };
        ready_receive.await.unwrap();
        let mut next = Box::pin(gate.acquire(model("next"), deadline()));
        assert!(poll(next.as_mut()).is_pending());
        active.abort();
        assert!(active.await.unwrap_err().is_cancelled());
        drop(ready(poll(next.as_mut())).unwrap());
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }

    #[tokio::test]
    async fn noncounting_interruption_remembers_pause_resume_before_polling() {
        let gate = gate();
        let old = gate.interruption();
        gate.pause(true);
        let paused = gate.interruption();
        gate.pause(false);
        assert_eq!(old.wait().await.code, "startup_paused");
        assert_eq!(paused.wait().await.code, "startup_paused");
        let fresh = gate.interruption();
        let mut waiting = Box::pin(fresh.wait());
        assert!(poll(waiting.as_mut()).is_pending());
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
        gate.shutdown();
        assert_eq!(ready(poll(waiting.as_mut())).code, "shutting_down");
    }

    #[tokio::test]
    async fn snapshots_bound_models_and_invalid_ids_never_enter_the_queue() {
        let gate = StartupGate::new(Settings {
            pending: 128,
            ..Default::default()
        })
        .unwrap();
        let held = gate.acquire(model("held"), deadline()).await.unwrap();
        let mut waiting = Vec::new();
        for index in 0..65 {
            let mut future =
                Box::pin(gate.acquire(Arc::from(format!("model-{index}")), deadline()));
            assert!(poll(future.as_mut()).is_pending());
            waiting.push(future);
        }
        let snapshot = gate.snapshot();
        assert_eq!(snapshot.pending, 65);
        assert_eq!(snapshot.pending_models.len(), 64);
        assert!(snapshot.models_truncated);
        for value in [
            "".to_owned(),
            "x\nprivate".into(),
            "x".repeat(257),
            "é".repeat(129),
        ] {
            assert_eq!(
                gate.acquire(Arc::from(value), deadline())
                    .await
                    .unwrap_err()
                    .code,
                "invalid_startup_model"
            );
        }
        drop(waiting);
        drop(held);
        assert_eq!((gate.snapshot().active, gate.snapshot().pending), (0, 0));
    }
}
