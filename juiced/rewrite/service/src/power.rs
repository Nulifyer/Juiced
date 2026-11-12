use serde::Serialize;
use std::{
    sync::{Arc, Mutex, Weak, mpsc},
    time::{Duration, Instant},
};
use tokio::sync::Notify;

#[cfg(target_os = "linux")]
#[path = "power/linux.rs"]
mod platform;

const IDLE_GRACE: Duration = Duration::from_millis(250);
const RETRY_DELAY: Duration = Duration::from_secs(5);
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);

type NativeLease = Box<dyn Send>;

trait Backend: Send + Sync + 'static {
    fn platform(&self) -> &'static str;
    fn supported(&self) -> bool {
        true
    }
    fn acquire(&self, deadline: Instant) -> Result<NativeLease, &'static str>;
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;
    pub(super) struct Native;
    impl Native {
        pub(super) fn new() -> Self {
            Self
        }
    }
    impl Backend for Native {
        fn platform(&self) -> &'static str {
            "unsupported"
        }
        fn supported(&self) -> bool {
            false
        }
        fn acquire(&self, _deadline: Instant) -> Result<NativeLease, &'static str> {
            Err("suspend_inhibition_unsupported")
        }
    }
}

#[cfg(not(test))]
type StateGuard<'a> = std::sync::MutexGuard<'a, State>;

#[cfg(test)]
std::thread_local! {
    static STATE_LOCK_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
struct StateGuard<'a>(std::sync::MutexGuard<'a, State>);

#[cfg(test)]
impl std::ops::Deref for StateGuard<'_> {
    type Target = State;
    fn deref(&self) -> &State {
        &self.0
    }
}

#[cfg(test)]
impl std::ops::DerefMut for StateGuard<'_> {
    fn deref_mut(&mut self) -> &mut State {
        &mut self.0
    }
}

#[cfg(test)]
impl Drop for StateGuard<'_> {
    fn drop(&mut self) {
        STATE_LOCK_DEPTH.with(|depth| depth.set(depth.get() - 1));
    }
}

struct State {
    active: usize,
    requested: Option<Instant>,
    pending: Option<u64>,
    generation: u64,
    held: bool,
    releasing: bool,
    idle_until: Option<Instant>,
    retry_after: Instant,
    shutting_down: bool,
    worker_running: bool,
    uncertain: bool,
    last_error: Option<&'static str>,
    attempts: u64,
    acquisitions: u64,
    releases: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub enabled: bool,
    pub platform: &'static str,
    pub active: usize,
    pub held: bool,
    pub acquiring: bool,
    pub releasing: bool,
    pub idle_grace: bool,
    pub shutting_down: bool,
    pub settled: bool,
    pub last_error: Option<&'static str>,
    pub attempts: u64,
    pub acquisitions: u64,
    pub releases: u64,
}

pub struct Controller {
    enabled: bool,
    platform: &'static str,
    state: Mutex<State>,
    changed: Notify,
    wake: mpsc::SyncSender<()>,
    idle_grace: Duration,
    retry_delay: Duration,
}

#[must_use = "Hold the activity until the protected operation and its response have finished"]
pub struct Activity {
    controller: Option<Arc<Controller>>,
}

impl Controller {
    pub fn new(enabled: bool) -> Arc<Self> {
        Self::with_backend(
            enabled,
            Arc::new(platform::Native::new()),
            IDLE_GRACE,
            RETRY_DELAY,
        )
    }

    fn with_backend(
        enabled: bool,
        backend: Arc<dyn Backend>,
        idle_grace: Duration,
        retry_delay: Duration,
    ) -> Arc<Self> {
        let (wake, receiver) = mpsc::sync_channel(1);
        let supported = backend.supported();
        let controller = Arc::new(Self {
            enabled,
            platform: backend.platform(),
            state: Mutex::new(State {
                active: 0,
                requested: None,
                pending: None,
                generation: 0,
                held: false,
                releasing: false,
                idle_until: None,
                retry_after: Instant::now(),
                shutting_down: false,
                worker_running: enabled && supported,
                uncertain: false,
                last_error: if enabled && !supported {
                    Some("suspend_inhibition_unsupported")
                } else {
                    None
                },
                attempts: 0,
                acquisitions: 0,
                releases: 0,
            }),
            changed: Notify::new(),
            wake,
            idle_grace,
            retry_delay,
        });
        if enabled && supported {
            let owner = Arc::downgrade(&controller);
            let failure_owner = owner.clone();
            let spawned = std::thread::Builder::new()
                .name("juiced-power".into())
                .spawn(move || {
                    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        run_worker(owner, backend, receiver)
                    }))
                    .is_err()
                        && let Some(owner) = failure_owner.upgrade()
                    {
                        let mut state = owner.lock();
                        state.worker_running = false;
                        state.uncertain = true;
                        state.last_error = Some("inhibitor_worker_failed");
                        drop(state);
                        owner.changed.notify_waiters();
                    }
                });
            if spawned.is_err() {
                let mut state = controller.lock();
                state.worker_running = false;
                state.last_error = Some("inhibitor_worker_unavailable");
            }
        }
        controller
    }

    /// A canceled wait releases its activity count; native work remains owned until it settles.
    pub async fn activity(self: &Arc<Self>, deadline: Instant) -> Activity {
        let (wake, wait) = {
            let mut state = self.lock();
            if state.shutting_down {
                return Activity { controller: None };
            }
            let Some(active) = state.active.checked_add(1) else {
                state.last_error = Some("inhibitor_activity_limit");
                return Activity { controller: None };
            };
            state.active = active;
            state.idle_until = None;
            let mut wake = false;
            if self.enabled
                && state.worker_running
                && !state.uncertain
                && !state.held
                && state.pending.is_none()
                && Instant::now() >= state.retry_after
                && Instant::now() < deadline
            {
                state.requested = Some(
                    state
                        .requested
                        .map_or(deadline, |prior| prior.max(deadline)),
                );
                wake = true;
            }
            let wait = state.pending.is_some() || state.requested.is_some();
            (wake, wait)
        };
        let activity = Activity {
            controller: Some(self.clone()),
        };
        if wake {
            self.wake();
        }
        if wait {
            loop {
                let notified = self.changed.notified();
                let done = {
                    let state = self.lock();
                    state.held
                        || state.shutting_down
                        || !state.worker_running
                        || (state.pending.is_none() && state.requested.is_none())
                };
                if done
                    || tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
                        .await
                        .is_err()
                {
                    break;
                }
            }
        }
        activity
    }

    pub fn snapshot(&self) -> Snapshot {
        let state = self.lock();
        Snapshot {
            schema_version: 1,
            enabled: self.enabled,
            platform: self.platform,
            active: state.active,
            held: state.held,
            acquiring: state.pending.is_some() || state.requested.is_some(),
            releasing: state.releasing,
            idle_grace: state.active == 0 && state.held && !state.shutting_down,
            shutting_down: state.shutting_down,
            settled: settled(&state),
            last_error: state.last_error,
            attempts: state.attempts,
            acquisitions: state.acquisitions,
            releases: state.releases,
        }
    }

    /// A false result retains pending cleanup; it never certifies a timed-out native call.
    pub async fn shutdown(&self, deadline: Instant) -> bool {
        {
            let mut state = self.lock();
            state.shutting_down = true;
            state.requested = None;
        }
        self.wake();
        loop {
            let notified = self.changed.notified();
            if settled(&self.lock()) {
                return true;
            }
            if tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), notified)
                .await
                .is_err()
            {
                return settled(&self.lock());
            }
        }
    }

    fn lock(&self) -> StateGuard<'_> {
        let guard = self.state.lock().unwrap_or_else(|poisoned| {
            let mut state = poisoned.into_inner();
            state.uncertain = true;
            state.last_error = Some("inhibitor_state_unavailable");
            state
        });
        #[cfg(test)]
        {
            STATE_LOCK_DEPTH.with(|depth| depth.set(depth.get() + 1));
            StateGuard(guard)
        }
        #[cfg(not(test))]
        guard
    }

    fn wake(&self) {
        let _ = self.wake.try_send(());
    }
}

impl Drop for Activity {
    fn drop(&mut self) {
        let Some(owner) = self.controller.take() else {
            return;
        };
        let mut state = owner.lock();
        state.active = state.active.saturating_sub(1);
        if state.active == 0 {
            state.idle_until = Some(
                Instant::now()
                    .checked_add(owner.idle_grace)
                    .unwrap_or_else(Instant::now),
            );
            state.requested = None;
            drop(state);
            owner.wake();
            owner.changed.notify_waiters();
        }
    }
}

fn settled(state: &State) -> bool {
    !state.uncertain
        && state.active == 0
        && !state.held
        && !state.releasing
        && state.pending.is_none()
        && state.requested.is_none()
        && (!state.shutting_down || !state.worker_running)
}

enum Decision {
    Acquire(u64, Instant),
    Release,
    Wait(Option<Duration>),
    Stop,
}

fn run_worker(owner: Weak<Controller>, backend: Arc<dyn Backend>, receiver: mpsc::Receiver<()>) {
    let mut resource: Option<NativeLease> = None;
    loop {
        let Some(controller) = owner.upgrade() else {
            break;
        };
        let decision = {
            let mut state = controller.lock();
            if resource.is_some() {
                if state.active == 0
                    && (state.shutting_down
                        || state
                            .idle_until
                            .is_some_and(|until| Instant::now() >= until))
                {
                    state.held = false;
                    state.releasing = true;
                    Decision::Release
                } else {
                    Decision::Wait(if state.active == 0 {
                        state
                            .idle_until
                            .map(|until| until.saturating_duration_since(Instant::now()))
                    } else {
                        None
                    })
                }
            } else if state.shutting_down && state.active == 0 {
                Decision::Stop
            } else if !state.shutting_down
                && state.active > 0
                && let Some(deadline) = state.requested.take()
            {
                if let Some(generation) = state.generation.checked_add(1) {
                    state.generation = generation;
                    state.pending = Some(generation);
                    state.attempts = state.attempts.saturating_add(1);
                    Decision::Acquire(generation, deadline.min(Instant::now() + ACQUIRE_TIMEOUT))
                } else {
                    state.last_error = Some("inhibitor_generation_exhausted");
                    controller.changed.notify_waiters();
                    Decision::Wait(None)
                }
            } else {
                Decision::Wait(None)
            }
        };
        match decision {
            Decision::Acquire(generation, deadline) => {
                drop(controller);
                let result = backend.acquire(deadline);
                let Some(controller) = owner.upgrade() else {
                    drop(result);
                    break;
                };
                let mut discard = None;
                {
                    let mut state = controller.lock();
                    if state.pending == Some(generation) {
                        state.pending = None;
                        match result {
                            Ok(lease) => {
                                state.acquisitions = state.acquisitions.saturating_add(1);
                                state.last_error = None;
                                if state.active > 0
                                    || (!state.shutting_down
                                        && state
                                            .idle_until
                                            .is_some_and(|until| Instant::now() < until))
                                {
                                    state.held = true;
                                    resource = Some(lease);
                                } else {
                                    state.releasing = true;
                                    discard = Some(lease);
                                }
                            }
                            Err(code) => {
                                state.last_error = Some(code);
                                state.retry_after = Instant::now()
                                    .checked_add(controller.retry_delay)
                                    .unwrap_or_else(Instant::now);
                            }
                        }
                    } else if let Ok(lease) = result {
                        discard = Some(lease);
                    }
                }
                controller.changed.notify_waiters();
                if let Some(lease) = discard {
                    drop(controller);
                    drop(lease);
                    let Some(controller) = owner.upgrade() else {
                        break;
                    };
                    let mut state = controller.lock();
                    if state.pending.is_none() && !state.held {
                        state.releasing = false;
                        state.releases = state.releases.saturating_add(1);
                    }
                    drop(state);
                    controller.changed.notify_waiters();
                }
            }
            Decision::Release => {
                drop(controller);
                drop(resource.take());
                let Some(controller) = owner.upgrade() else {
                    break;
                };
                let mut state = controller.lock();
                state.releasing = false;
                state.releases = state.releases.saturating_add(1);
                drop(state);
                controller.changed.notify_waiters();
            }
            Decision::Wait(timeout) => {
                // A strong owner here would keep its sender alive after final Controller drop.
                drop(controller);
                let result = match timeout {
                    Some(timeout) => receiver.recv_timeout(timeout),
                    None => receiver
                        .recv()
                        .map_err(|_| mpsc::RecvTimeoutError::Disconnected),
                };
                if matches!(result, Err(mpsc::RecvTimeoutError::Disconnected)) {
                    break;
                }
            }
            Decision::Stop => break,
        }
    }
    drop(resource);
    drop(backend);
    if let Some(controller) = owner.upgrade() {
        let mut state = controller.lock();
        state.worker_running = false;
        drop(state);
        controller.changed.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Condvar,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    };

    struct Gate {
        permits: Mutex<usize>,
        changed: Condvar,
    }
    impl Gate {
        fn new(permits: usize) -> Self {
            Self {
                permits: Mutex::new(permits),
                changed: Condvar::new(),
            }
        }
        fn release(&self) {
            *self.permits.lock().unwrap() += 1;
            self.changed.notify_one();
        }
        fn enter(&self) {
            let mut permits = self.permits.lock().unwrap();
            while *permits == 0 {
                permits = self.changed.wait(permits).unwrap();
            }
            *permits -= 1;
        }
    }

    #[derive(Default)]
    struct Counts {
        attempts: AtomicUsize,
        failures: AtomicUsize,
        live: AtomicUsize,
        maximum_live: AtomicUsize,
        dropping: AtomicUsize,
        dropped: AtomicUsize,
        wrong_thread: AtomicBool,
        owner_locked_on_drop: AtomicBool,
    }
    struct Fake {
        acquire: Gate,
        release: Arc<Gate>,
        counts: Arc<Counts>,
    }
    struct Held {
        counts: Arc<Counts>,
        release: Arc<Gate>,
    }
    impl Fake {
        fn new(permits: usize) -> Arc<Self> {
            Arc::new(Self {
                acquire: Gate::new(permits),
                release: Arc::new(Gate::new(100)),
                counts: Arc::new(Counts::default()),
            })
        }
        fn controller(
            self: &Arc<Self>,
            enabled: bool,
            grace: Duration,
            retry: Duration,
        ) -> Arc<Controller> {
            Controller::with_backend(enabled, self.clone(), grace, retry)
        }
    }
    impl Backend for Fake {
        fn platform(&self) -> &'static str {
            "controlled-native"
        }
        fn acquire(&self, _deadline: Instant) -> Result<NativeLease, &'static str> {
            self.counts.attempts.fetch_add(1, Ordering::SeqCst);
            if std::thread::current().name() != Some("juiced-power") {
                self.counts.wrong_thread.store(true, Ordering::SeqCst);
            }
            self.acquire.enter();
            if self
                .counts
                .failures
                .try_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                    value.checked_sub(1)
                })
                .is_ok()
            {
                return Err("controlled_acquisition_unavailable");
            }
            let live = self.counts.live.fetch_add(1, Ordering::SeqCst) + 1;
            self.counts.maximum_live.fetch_max(live, Ordering::SeqCst);
            Ok(Box::new(Held {
                counts: self.counts.clone(),
                release: self.release.clone(),
            }))
        }
    }
    impl Drop for Held {
        fn drop(&mut self) {
            self.counts.dropping.fetch_add(1, Ordering::SeqCst);
            if std::thread::current().name() != Some("juiced-power") {
                self.counts.wrong_thread.store(true, Ordering::SeqCst);
            }
            if STATE_LOCK_DEPTH.with(|depth| depth.get() != 0) {
                self.counts
                    .owner_locked_on_drop
                    .store(true, Ordering::SeqCst);
            }
            self.release.enter();
            self.counts.live.fetch_sub(1, Ordering::SeqCst);
            self.counts.dropped.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }
    async fn until(mut predicate: impl FnMut() -> bool) {
        let limit = deadline();
        while !predicate() {
            assert!(
                Instant::now() < limit,
                "controlled native state did not settle"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    fn verify_native_thread(fake: &Fake) {
        assert!(!fake.counts.wrong_thread.load(Ordering::SeqCst));
        assert!(!fake.counts.owner_locked_on_drop.load(Ordering::SeqCst));
        assert!(fake.counts.maximum_live.load(Ordering::SeqCst) <= 1);
    }

    #[test]
    fn native_drop_probe_distinguishes_current_thread_from_concurrent_lock_owners() {
        for same_thread in [false, true] {
            let fake = Fake::new(0);
            let owner = fake.controller(false, IDLE_GRACE, RETRY_DELAY);
            let held_elsewhere = (!same_thread).then(|| owner.lock());
            let worker_owner = owner.clone();
            let counts = fake.counts.clone();
            let release = fake.release.clone();
            counts.live.store(1, Ordering::SeqCst);
            std::thread::Builder::new()
                .name("juiced-power".into())
                .spawn(move || {
                    let _held_here = same_thread.then(|| worker_owner.lock());
                    assert!(worker_owner.state.try_lock().is_err());
                    drop(Held { counts, release });
                })
                .unwrap()
                .join()
                .unwrap();
            drop(held_elsewhere);
            assert_eq!(
                fake.counts.owner_locked_on_drop.load(Ordering::SeqCst),
                same_thread
            );
            assert!(!fake.counts.wrong_thread.load(Ordering::SeqCst));
            assert_eq!(fake.counts.live.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn disabled_activity_does_not_start_native_work() {
        let fake = Fake::new(0);
        let owner = fake.controller(false, IDLE_GRACE, RETRY_DELAY);
        let activity = owner.activity(deadline()).await;
        assert_eq!(owner.snapshot().active, 1);
        assert!(!owner.snapshot().held && !owner.snapshot().acquiring);
        assert_eq!(fake.counts.attempts.load(Ordering::SeqCst), 0);
        assert!(!owner.lock().worker_running);
        drop(activity);
        assert!(owner.shutdown(deadline()).await);
        assert!(owner.snapshot().settled);
    }

    #[tokio::test]
    async fn concurrent_cold_waiters_share_one_native_acquisition_without_missed_notifications() {
        let fake = Fake::new(0);
        let owner = fake.controller(true, IDLE_GRACE, RETRY_DELAY);
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..64 {
            let owner = owner.clone();
            tasks.spawn(async move { owner.activity(deadline()).await });
        }
        until(|| owner.snapshot().active == 64 && fake.counts.attempts.load(Ordering::SeqCst) == 1)
            .await;
        assert!(owner.snapshot().acquiring && !owner.snapshot().held);
        assert_eq!(owner.snapshot().acquisitions, 0);
        fake.acquire.release();
        let activities = tokio::time::timeout(Duration::from_secs(1), async {
            let mut activities = Vec::new();
            while let Some(result) = tasks.join_next().await {
                activities.push(result.unwrap());
            }
            activities
        })
        .await
        .expect("all acknowledged waiters must wake before their acquisition deadline");
        assert_eq!(activities.len(), 64);
        assert_eq!(owner.snapshot().acquisitions, 1);
        assert_eq!(fake.counts.attempts.load(Ordering::SeqCst), 1);
        drop(activities);
        assert!(owner.shutdown(deadline()).await);
        assert_eq!(owner.snapshot().releases, 1);
        verify_native_thread(&fake);
    }

    #[tokio::test]
    async fn concurrent_burst_reuses_one_acknowledged_resource_and_stale_idle_wakes_preserve_it() {
        let fake = Fake::new(2);
        let owner = fake.controller(true, Duration::from_millis(40), RETRY_DELAY);
        let first = owner.activity(deadline()).await;
        let mut tasks = tokio::task::JoinSet::new();
        for _ in 0..64 {
            let owner = owner.clone();
            tasks.spawn(async move { owner.activity(deadline()).await });
        }
        let mut activities = Vec::new();
        while let Some(result) = tasks.join_next().await {
            activities.push(result.unwrap());
        }
        assert_eq!(owner.snapshot().active, 65);
        assert_eq!(owner.snapshot().acquisitions, 1);
        assert_eq!(fake.counts.attempts.load(Ordering::SeqCst), 1);
        drop(first);
        drop(activities);
        assert!(owner.snapshot().idle_grace);
        let next = owner.activity(deadline()).await;
        tokio::time::sleep(Duration::from_millis(90)).await;
        assert!(owner.snapshot().held);
        assert_eq!(fake.counts.dropped.load(Ordering::SeqCst), 0);
        drop(next);
        until(|| owner.snapshot().settled).await;
        assert_eq!(fake.counts.dropped.load(Ordering::SeqCst), 1);
        let second_burst = owner.activity(deadline()).await;
        assert_eq!(owner.snapshot().acquisitions, 2);
        drop(second_burst);
        assert!(owner.shutdown(deadline()).await);
        assert_eq!(owner.snapshot().releases, 2);
        verify_native_thread(&fake);
    }

    #[tokio::test]
    async fn timed_out_wait_returns_activity_and_retains_pending_cleanup_until_ack() {
        let fake = Fake::new(0);
        let owner = fake.controller(true, Duration::from_millis(5), RETRY_DELAY);
        let activity = owner
            .activity(Instant::now() + Duration::from_millis(20))
            .await;
        assert_eq!(owner.snapshot().active, 1);
        assert!(owner.snapshot().acquiring && !owner.snapshot().held);
        assert_eq!(owner.snapshot().acquisitions, 0);
        drop(activity);
        assert_eq!(owner.snapshot().active, 0);
        assert!(
            !owner
                .shutdown(Instant::now() + Duration::from_millis(15))
                .await
        );
        assert!(owner.snapshot().acquiring);
        fake.acquire.release();
        assert!(owner.shutdown(deadline()).await);
        assert_eq!(owner.snapshot().acquisitions, 1);
        assert_eq!(owner.snapshot().releases, 1);
        assert!(!owner.snapshot().held);
        verify_native_thread(&fake);
    }

    #[tokio::test]
    async fn canceled_wait_does_not_release_a_later_activity_on_the_same_pending_acquisition() {
        let fake = Fake::new(0);
        let owner = fake.controller(true, Duration::from_millis(10), RETRY_DELAY);
        let waiting = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.activity(deadline()).await })
        };
        until(|| fake.counts.attempts.load(Ordering::SeqCst) == 1).await;
        assert!(!owner.snapshot().held);
        waiting.abort();
        assert!(waiting.await.err().unwrap().is_cancelled());
        assert_eq!(owner.snapshot().active, 0);
        let later = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.activity(deadline()).await })
        };
        until(|| owner.snapshot().active == 1).await;
        fake.acquire.release();
        let later = later.await.unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(owner.snapshot().held);
        assert_eq!(fake.counts.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(fake.counts.dropped.load(Ordering::SeqCst), 0);
        drop(later);
        assert!(owner.shutdown(deadline()).await);
        assert_eq!(owner.snapshot().releases, 1);
        verify_native_thread(&fake);
    }

    #[tokio::test]
    async fn blocked_native_release_keeps_new_acquisition_separate_and_never_blocks_owner_lock() {
        let fake = Fake::new(2);
        *fake.release.permits.lock().unwrap() = 0;
        let owner = fake.controller(true, Duration::from_millis(5), RETRY_DELAY);
        let first = owner.activity(deadline()).await;
        drop(first);
        until(|| fake.counts.dropping.load(Ordering::SeqCst) == 1).await;
        let snapshot = owner.snapshot();
        assert!(snapshot.releasing && !snapshot.held && !snapshot.settled);
        let next = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.activity(deadline()).await })
        };
        until(|| owner.snapshot().active == 1).await;
        assert_eq!(fake.counts.attempts.load(Ordering::SeqCst), 1);
        fake.release.release();
        let next = next.await.unwrap();
        assert!(owner.snapshot().held);
        assert_eq!(owner.snapshot().acquisitions, 2);
        assert_eq!(owner.snapshot().releases, 1);
        fake.release.release();
        drop(next);
        assert!(owner.shutdown(deadline()).await);
        assert_eq!(owner.snapshot().releases, 2);
        verify_native_thread(&fake);
    }

    #[tokio::test]
    async fn failed_acquisition_is_best_effort_backed_off_and_retryable() {
        let fake = Fake::new(2);
        fake.counts.failures.store(1, Ordering::SeqCst);
        let owner = fake.controller(true, Duration::from_millis(5), Duration::from_secs(1));
        let first = owner.activity(deadline()).await;
        assert!(!owner.snapshot().held);
        assert_eq!(
            owner.snapshot().last_error,
            Some("controlled_acquisition_unavailable")
        );
        let mut activities = Vec::new();
        for _ in 0..32 {
            activities.push(owner.activity(deadline()).await);
        }
        assert_eq!(fake.counts.attempts.load(Ordering::SeqCst), 1);
        assert_eq!(owner.snapshot().acquisitions, 0);
        tokio::time::sleep(Duration::from_millis(1010)).await;
        let retried = owner.activity(deadline()).await;
        assert!(owner.snapshot().held);
        assert_eq!(owner.snapshot().attempts, 2);
        assert_eq!(owner.snapshot().acquisitions, 1);
        assert_eq!(owner.snapshot().last_error, None);
        drop(first);
        drop(activities);
        drop(retried);
        assert!(owner.shutdown(deadline()).await);
        verify_native_thread(&fake);
    }

    #[tokio::test]
    async fn shutdown_waits_for_existing_activity_and_refuses_new_acquisition() {
        let fake = Fake::new(1);
        let owner = fake.controller(true, IDLE_GRACE, RETRY_DELAY);
        let activity = owner.activity(deadline()).await;
        assert!(
            !owner
                .shutdown(Instant::now() + Duration::from_millis(15))
                .await
        );
        let refused = owner.activity(deadline()).await;
        assert_eq!(owner.snapshot().active, 1);
        assert_eq!(owner.snapshot().attempts, 1);
        assert!(owner.snapshot().held && owner.snapshot().shutting_down);
        drop(refused);
        drop(activity);
        assert!(owner.shutdown(deadline()).await);
        assert!(owner.snapshot().settled);
        assert_eq!(fake.counts.dropped.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn final_owner_drop_never_joins_a_blocked_native_destructor() {
        let fake = Fake::new(1);
        *fake.release.permits.lock().unwrap() = 0;
        let owner = fake.controller(true, Duration::from_millis(5), RETRY_DELAY);
        let activity = owner.activity(deadline()).await;
        let weak = Arc::downgrade(&owner);
        drop(activity);
        until(|| fake.counts.dropping.load(Ordering::SeqCst) == 1).await;
        drop(owner);
        assert!(weak.upgrade().is_none());
        assert_eq!(fake.counts.live.load(Ordering::SeqCst), 1);
        fake.release.release();
        until(|| fake.counts.dropped.load(Ordering::SeqCst) == 1).await;
        verify_native_thread(&fake);
    }

    #[tokio::test]
    async fn final_owner_cancellation_keeps_pending_native_acquisition_owned_without_arc_cycle() {
        let fake = Fake::new(0);
        let owner = fake.controller(true, IDLE_GRACE, RETRY_DELAY);
        let weak = Arc::downgrade(&owner);
        let waiting = {
            let owner = owner.clone();
            tokio::spawn(async move { owner.activity(deadline()).await })
        };
        until(|| fake.counts.attempts.load(Ordering::SeqCst) == 1).await;
        waiting.abort();
        assert!(waiting.await.err().unwrap().is_cancelled());
        drop(owner);
        assert!(weak.upgrade().is_none());
        fake.acquire.release();
        until(|| fake.counts.dropped.load(Ordering::SeqCst) == 1).await;
        verify_native_thread(&fake);
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    #[ignore = "requires an accessible system bus/logind and sleep:idle block-inhibitor permission"]
    async fn real_logind_inhibitor_round_trip() {
        let owner = Controller::new(true);
        let activity = owner.activity(deadline()).await;
        let snapshot = owner.snapshot();
        assert!(
            snapshot.held && snapshot.acquisitions == 1,
            "OS inhibitor was not acknowledged: {:?}",
            snapshot.last_error
        );
        let shared = owner.activity(deadline()).await;
        assert_eq!(owner.snapshot().acquisitions, 1);
        drop(activity);
        drop(shared);
        assert!(owner.shutdown(deadline()).await);
        let snapshot = owner.snapshot();
        assert!(snapshot.settled && !snapshot.held && !snapshot.acquiring && !snapshot.releasing);
        assert_eq!(snapshot.active, 0);
        assert_eq!(snapshot.acquisitions, 1);
        assert_eq!(snapshot.releases, 1);
    }
}
