use serde::Serialize;
use std::{
    collections::BTreeMap,
    fmt, io,
    process::{Child, ExitStatus},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::process::Command;

pub const DEFAULT_STOP_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Phase {
    Starting,
    Running,
    Stopping,
    StopFailed,
    CleanupPending,
    Cleaning,
}

#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub model: Arc<str>,
    pub generation: u64,
    pub pid: Option<u32>,
    pub phase: Phase,
    pub last_error: Option<&'static str>,
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
    fn waiting() -> Self {
        Self::new(
            502,
            "process_wait",
            "The owned child process exit could not be confirmed. Its resources remain reserved.",
        )
    }
    fn stopping() -> Self {
        Self::new(
            502,
            "process_stop",
            "The owned child process could not be stopped. Its resources remain reserved for retry.",
        )
    }
    fn timeout() -> Self {
        Self::new(
            504,
            "process_stop_timeout",
            "The owned child process has not confirmed exit within the stop deadline. Its resources remain reserved.",
        )
    }
}
impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}

/// Kills what remains of the worker's process group. The group id is the worker's pid,
/// which the kernel does not hand to a new process while the group has members.
fn signal_group(child: &Child) {
    #[cfg(unix)]
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill(2) with a negative pid only sends a signal; it touches no memory.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = child;
}

struct Owned<R> {
    child: Child,
    resources: R,
    exited: Option<ExitStatus>,
}
struct Record<R> {
    pid: Option<u32>,
    phase: Phase,
    last_error: Option<&'static str>,
    pending: Option<Owned<R>>,
}
struct State<R> {
    next_generation: u64,
    models: BTreeMap<Arc<str>, BTreeMap<u64, Record<R>>>,
}

pub struct Registry<R: Send + 'static> {
    timeout: Duration,
    state: Mutex<State<R>>,
    #[cfg(test)]
    faults: Faults,
}

pub struct ManagedChild<R: Send + 'static> {
    registry: Arc<Registry<R>>,
    model: Arc<str>,
    generation: u64,
    owned: Option<Owned<R>>,
}
impl<R: Send + 'static> fmt::Debug for ManagedChild<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagedChild")
            .field("model", &self.model)
            .field("generation", &self.generation)
            .field("pid", &self.id())
            .finish()
    }
}

struct CleanupAttempt<R: Send + 'static> {
    registry: Arc<Registry<R>>,
    model: Arc<str>,
    generation: u64,
    owned: Option<Owned<R>>,
    error: &'static str,
}
struct StopAttempt<R: Send + 'static> {
    registry: Arc<Registry<R>>,
    model: Arc<str>,
    generation: u64,
    error: &'static str,
    finished: bool,
}

#[cfg(test)]
#[derive(Default)]
struct Faults {
    kill: std::sync::atomic::AtomicBool,
    wait: std::sync::atomic::AtomicBool,
    withhold_ack: std::sync::atomic::AtomicBool,
    pause: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Notify,
    resume: tokio::sync::Notify,
    background_finished: tokio::sync::Notify,
}

impl<R: Send + 'static> Registry<R> {
    #[cfg(all(test, unix))]
    pub(crate) fn test_fail_kill(&self, fail: bool) {
        self.faults
            .kill
            .store(fail, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, unix))]
    pub(crate) fn test_withhold_ack(&self, withhold: bool) {
        self.faults
            .withhold_ack
            .store(withhold, std::sync::atomic::Ordering::Release);
    }

    #[cfg(all(test, unix))]
    pub(crate) fn test_pause_stop(&self, pause: bool) {
        self.faults
            .pause
            .store(pause, std::sync::atomic::Ordering::Release);
        if !pause {
            self.faults.resume.notify_waiters();
        }
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn test_stop_entered(&self) {
        self.faults.entered.notified().await;
    }

    #[cfg(all(test, unix))]
    pub(crate) async fn test_background_finished(&self) {
        self.faults.background_finished.notified().await;
    }

    pub fn new(stop_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            timeout: stop_timeout,
            state: Mutex::new(State {
                next_generation: 0,
                models: BTreeMap::new(),
            }),
            #[cfg(test)]
            faults: Faults::default(),
        })
    }

    pub fn spawn(
        self: &Arc<Self>,
        model: Arc<str>,
        command: &mut Command,
        resources: R,
    ) -> Result<ManagedChild<R>, Error> {
        if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            return Err(Error::new(
                400,
                "invalid_process_model",
                "Provide a bounded model identifier for the owned child process.",
            ));
        }
        let generation = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            if state
                .models
                .get(model.as_ref())
                .is_some_and(|records| !records.is_empty())
            {
                Err(Error::new(
                    409,
                    "process_generation_busy",
                    "This model still has an owned child process or pending cleanup. Confirm its exit before restarting.",
                ))
            } else if let Some(next) = state.next_generation.checked_add(1) {
                state.next_generation = next;
                state.models.entry(model.clone()).or_default().insert(
                    next,
                    Record {
                        pid: None,
                        phase: Phase::Starting,
                        last_error: None,
                        pending: None,
                    },
                );
                Ok(next)
            } else {
                Err(Error::new(
                    503,
                    "process_generation_exhausted",
                    "The child process generation counter cannot advance.",
                ))
            }
        }?;
        // The registry owns every stop and exit acknowledgement, including outside Tokio runtimes.
        command.kill_on_drop(false);
        // Engines such as vLLM run their model in child processes; a group of its own lets
        // stop reach them, since they outlive their parent.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(command.as_std_mut(), 0);
        let child = match command.as_std_mut().spawn() {
            Ok(child) => child,
            Err(_) => {
                self.confirmed(&model, generation);
                return Err(Error::new(
                    502,
                    "process_spawn",
                    "The child process could not start. Check its executable and launch settings.",
                ));
            }
        };
        let pid = child.id();
        self.update(&model, generation, Phase::Running, None, Some(pid));
        Ok(ManagedChild {
            registry: self.clone(),
            model,
            generation,
            owned: Some(Owned {
                child,
                resources,
                exited: None,
            }),
        })
    }

    pub fn snapshot(&self, model: &str) -> Option<Observation> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let (model, records) = state.models.get_key_value(model)?;
        let (generation, record) = records.first_key_value()?;
        Some(Self::observation(model, *generation, record))
    }

    pub fn snapshots(&self) -> Vec<Observation> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state
            .models
            .iter()
            .flat_map(|(model, records)| {
                records
                    .iter()
                    .map(|(generation, record)| Self::observation(model, *generation, record))
            })
            .collect()
    }

    fn observation(model: &Arc<str>, generation: u64, record: &Record<R>) -> Observation {
        Observation {
            model: model.clone(),
            generation,
            pid: record.pid,
            phase: record.phase,
            last_error: record.last_error,
        }
    }
    fn update(
        &self,
        model: &str,
        generation: u64,
        phase: Phase,
        error: Option<&'static str>,
        pid: Option<u32>,
    ) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(record) = state
            .models
            .get_mut(model)
            .and_then(|records| records.get_mut(&generation))
        {
            record.phase = phase;
            record.last_error = error;
            if pid.is_some() {
                record.pid = pid;
            }
        }
    }
    fn confirmed(&self, model: &str, generation: u64) {
        let removed = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let removed = state.models.get_mut(model).and_then(|records| {
                if records
                    .get(&generation)
                    .is_some_and(|record| record.pending.is_none())
                {
                    records.remove(&generation)
                } else {
                    None
                }
            });
            if state.models.get(model).is_some_and(BTreeMap::is_empty) {
                state.models.remove(model);
            }
            removed
        };
        drop(removed);
    }
    fn restore(&self, model: Arc<str>, generation: u64, owned: Owned<R>, error: &'static str) {
        let mut owned = Some(owned);
        {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let record = state
                .models
                .entry(model)
                .or_default()
                .entry(generation)
                .or_insert_with(|| Record {
                    pid: None,
                    phase: Phase::CleanupPending,
                    last_error: None,
                    pending: None,
                });
            if record.pending.is_none() {
                let value = owned.take().expect("the cleanup owner is present");
                record.pid = Some(value.child.id());
                record.phase = Phase::CleanupPending;
                record.last_error = Some(error);
                record.pending = Some(value);
            }
        }
        // Duplicate owners cannot arise through this API; retain uncertainty if an invariant fails.
        if let Some(owned) = owned {
            std::mem::forget(owned);
        }
    }
    fn poll_exit(&self, owned: &mut Owned<R>) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = owned.exited {
            return Ok(Some(status));
        }
        #[cfg(test)]
        if self.faults.wait.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other("injected owned-child wait failure"));
        }
        #[cfg(test)]
        if self
            .faults
            .withhold_ack
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Ok(None);
        }
        let status = owned.child.try_wait()?;
        if let Some(status) = status {
            if owned.exited.is_none() {
                signal_group(&owned.child);
            }
            owned.exited = Some(status);
        }
        Ok(status)
    }
    fn kill(&self, owned: &mut Owned<R>) -> io::Result<()> {
        #[cfg(test)]
        if self.faults.kill.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::other("injected owned-child stop failure"));
        }
        signal_group(&owned.child);
        owned.child.kill()
    }
    async fn stop_owned(&self, owned: &mut Owned<R>) -> Result<ExitStatus, Error> {
        if let Some(status) = self.poll_exit(owned).map_err(|_| Error::waiting())? {
            return Ok(status);
        }
        #[cfg(test)]
        while self.faults.pause.load(std::sync::atomic::Ordering::Acquire) {
            self.faults.entered.notify_one();
            self.faults.resume.notified().await;
        }
        if self.kill(owned).is_err() {
            if let Some(status) = self.poll_exit(owned).map_err(|_| Error::waiting())? {
                return Ok(status);
            }
            return Err(Error::stopping());
        }
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .ok_or_else(Error::timeout)?;
        loop {
            if let Some(status) = self.poll_exit(owned).map_err(|_| Error::waiting())? {
                return Ok(status);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::timeout());
            }
            if tokio::runtime::Handle::try_current().is_err() {
                return Err(Error::new(
                    503,
                    "process_runtime_unavailable",
                    "The process exit is still unconfirmed and no cleanup runtime is available. Its resources remain reserved.",
                ));
            }
            tokio::time::sleep(remaining.min(Duration::from_millis(5))).await;
        }
    }
    fn schedule(self: &Arc<Self>, model: Arc<str>, generation: u64) {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let registry = self.clone();
            runtime.spawn(async move {
                let _ = registry.retry_generation(model, generation).await;
                #[cfg(test)]
                registry.faults.background_finished.notify_one();
            });
        } else {
            self.update(
                &model,
                generation,
                Phase::CleanupPending,
                Some("process_runtime_unavailable"),
                None,
            );
        }
    }
    pub async fn retry_cleanup(self: &Arc<Self>, model: &str) -> Result<(), Error> {
        let selected = {
            let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            match state.models.get_key_value(model) {
                None => return Ok(()),
                Some((model, records)) => {
                    if let Some((generation, _)) =
                        records.iter().find(|(_, record)| record.pending.is_some())
                    {
                        Ok((model.clone(), *generation))
                    } else if records
                        .values()
                        .any(|record| record.phase == Phase::Cleaning)
                    {
                        Err(Error::new(
                            409,
                            "process_cleanup_in_progress",
                            "Another cleanup attempt still owns this child process and its resources.",
                        ))
                    } else {
                        Err(Error::new(
                            409,
                            "process_guard_owned",
                            "The live child guard still owns this process. Stop that exact guard before retrying stored cleanup.",
                        ))
                    }
                }
            }
        }?;
        self.retry_generation(selected.0, selected.1).await
    }
    async fn retry_generation(
        self: &Arc<Self>,
        model: Arc<str>,
        generation: u64,
    ) -> Result<(), Error> {
        let owned = {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            let Some(record) = state
                .models
                .get_mut(model.as_ref())
                .and_then(|records| records.get_mut(&generation))
            else {
                return Ok(());
            };
            let owned = record.pending.take().ok_or_else(|| {
                Error::new(
                    409,
                    "process_cleanup_in_progress",
                    "Another cleanup owner still holds this child process and its resources.",
                )
            })?;
            record.phase = Phase::Cleaning;
            record.last_error = None;
            owned
        };
        let mut attempt = CleanupAttempt {
            registry: self.clone(),
            model,
            generation,
            owned: Some(owned),
            error: "process_cleanup_interrupted",
        };
        let result = self
            .stop_owned(
                attempt
                    .owned
                    .as_mut()
                    .expect("the cleanup child is present"),
            )
            .await;
        match result {
            Ok(_) => {
                self.confirmed(&attempt.model, generation);
                let owned = attempt.owned.take();
                drop(owned);
                Ok(())
            }
            Err(error) => {
                attempt.error = error.code;
                Err(error)
            }
        }
    }
}

impl<R: Send + 'static> ManagedChild<R> {
    pub fn id(&self) -> Option<u32> {
        self.owned
            .as_ref()
            .filter(|owned| owned.exited.is_none())
            .map(|owned| owned.child.id())
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn take_output(
        &mut self,
    ) -> (
        Option<std::process::ChildStdout>,
        Option<std::process::ChildStderr>,
    ) {
        self.owned.as_mut().map_or((None, None), |owned| {
            (owned.child.stdout.take(), owned.child.stderr.take())
        })
    }
    pub fn resources(&self) -> &R {
        &self
            .owned
            .as_ref()
            .expect("the live guard retains its resources")
            .resources
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self.registry.poll_exit(
            self.owned
                .as_mut()
                .expect("the child guard retains its handle"),
        );
        match &result {
            Ok(Some(_)) => self.registry.confirmed(&self.model, self.generation),
            Err(_) => self.registry.update(
                &self.model,
                self.generation,
                Phase::StopFailed,
                Some("process_wait"),
                None,
            ),
            Ok(None) => {}
        }
        result
    }
    pub async fn stop(&mut self) -> Result<(), Error> {
        self.registry
            .update(&self.model, self.generation, Phase::Stopping, None, None);
        let mut attempt = StopAttempt {
            registry: self.registry.clone(),
            model: self.model.clone(),
            generation: self.generation,
            error: "process_stop_interrupted",
            finished: false,
        };
        let result = self
            .registry
            .stop_owned(
                self.owned
                    .as_mut()
                    .expect("the child guard retains its handle"),
            )
            .await;
        match result {
            Ok(_) => {
                self.registry.confirmed(&self.model, self.generation);
                attempt.finished = true;
                Ok(())
            }
            Err(error) => {
                attempt.error = error.code;
                Err(error)
            }
        }
    }
}
impl<R: Send + 'static> Drop for ManagedChild<R> {
    fn drop(&mut self) {
        let Some(mut owned) = self.owned.take() else {
            return;
        };
        let reason = match self.registry.poll_exit(&mut owned) {
            Ok(Some(_)) => {
                self.registry.confirmed(&self.model, self.generation);
                drop(owned);
                return;
            }
            Ok(None) => {
                if self.registry.kill(&mut owned).is_ok() {
                    "process_exit_unconfirmed"
                } else {
                    "process_stop"
                }
            }
            Err(_) => "process_wait",
        };
        self.registry
            .restore(self.model.clone(), self.generation, owned, reason);
        self.registry.schedule(self.model.clone(), self.generation);
    }
}
impl<R: Send + 'static> Drop for CleanupAttempt<R> {
    fn drop(&mut self) {
        if let Some(owned) = self.owned.take() {
            self.registry
                .restore(self.model.clone(), self.generation, owned, self.error);
        }
    }
}
impl<R: Send + 'static> Drop for StopAttempt<R> {
    fn drop(&mut self) {
        if !self.finished {
            self.registry.update(
                &self.model,
                self.generation,
                Phase::StopFailed,
                Some(self.error),
                None,
            );
        }
    }
}
impl<R: Send + 'static> Drop for Registry<R> {
    fn drop(&mut self) {
        let models = std::mem::take(
            &mut self
                .state
                .get_mut()
                .unwrap_or_else(|error| error.into_inner())
                .models,
        );
        for (_, records) in models {
            for (_, record) in records {
                if let Some(mut owned) = record.pending {
                    if matches!(owned.child.try_wait(), Ok(Some(_))) {
                        drop(owned);
                    }
                    // Last-owner teardown cannot turn unconfirmed exit into released accounting.
                    else {
                        std::mem::forget(owned);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::FutureExt;
    use std::{
        io::{BufRead, BufReader, Write},
        process::Stdio,
        sync::{
            Weak,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct Probe {
        drops: Arc<AtomicUsize>,
        registry: Weak<Registry<Probe>>,
    }
    impl Drop for Probe {
        fn drop(&mut self) {
            if let Some(registry) = self.registry.upgrade() {
                let started = Instant::now();
                loop {
                    if registry.state.try_lock().is_ok() {
                        break;
                    }
                    assert!(
                        started.elapsed() < Duration::from_secs(1),
                        "Resource destruction ran under the process registry lock"
                    );
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    fn probe(registry: &Arc<Registry<Probe>>, drops: &Arc<AtomicUsize>) -> Probe {
        Probe {
            drops: drops.clone(),
            registry: Arc::downgrade(registry),
        }
    }
    fn command() -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--nocapture", "process_fixture_child"])
            .env("JUICED_PROCESS_FIXTURE_CHILD", "1")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        command
    }
    fn spawn(
        registry: &Arc<Registry<Probe>>,
        model: &str,
        drops: &Arc<AtomicUsize>,
    ) -> ManagedChild<Probe> {
        let mut child = registry
            .spawn(Arc::from(model), &mut command(), probe(registry, drops))
            .unwrap();
        let mut output = BufReader::new(child.owned.as_mut().unwrap().child.stdout.take().unwrap());
        loop {
            let mut line = String::new();
            assert!(output.read_line(&mut line).unwrap() > 0);
            if line.contains("process fixture ready") {
                break;
            }
        }
        child
    }
    async fn phase(
        registry: &Registry<Probe>,
        model: &str,
        expected: Phase,
        last_error: Option<&str>,
    ) {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let observed = registry.snapshot(model).unwrap();
            if observed.phase == expected
                && last_error.is_none_or(|error| observed.last_error == Some(error))
            {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "The process cleanup phase did not settle"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
    #[test]
    fn process_fixture_child() {
        if std::env::var_os("JUICED_PROCESS_FIXTURE_CHILD").is_some() {
            println!("process fixture ready");
            std::io::stdout().flush().unwrap();
            std::thread::sleep(Duration::from_secs(60));
        }
    }
    #[tokio::test]
    async fn owned_stop_confirms_exit_and_resource_drop_does_not_hold_registry_lock() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut child = spawn(&registry, "owned", &drops);
        assert!(child.id().is_some());
        assert_eq!(registry.snapshot("owned").unwrap().phase, Phase::Running);
        assert_eq!(child.resources().drops.load(Ordering::SeqCst), 0);
        child.stop().await.unwrap();
        assert!(child.id().is_none());
        assert!(child.try_wait().unwrap().is_some());
        assert!(registry.snapshot("owned").is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(child);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn dropped_startup_without_runtime_keeps_exact_child_and_resources_for_later_retry() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let child = spawn(&registry, "startup", &drops);
        let pid = child.id();
        let generation = child.generation();
        let started = Instant::now();
        drop(child);
        assert!(started.elapsed() < Duration::from_millis(250));
        let observed = registry.snapshot("startup").unwrap();
        assert_eq!(
            (observed.generation, observed.pid, observed.phase),
            (generation, pid, Phase::CleanupPending)
        );
        assert_eq!(observed.last_error, Some("process_runtime_unavailable"));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        let replacement_drops = Arc::new(AtomicUsize::new(0));
        assert_eq!(
            registry
                .spawn(
                    Arc::from("startup"),
                    &mut command(),
                    probe(&registry, &replacement_drops)
                )
                .unwrap_err()
                .code,
            "process_generation_busy"
        );
        assert_eq!(replacement_drops.load(Ordering::SeqCst), 1);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(registry.retry_cleanup("startup")).unwrap();
        assert!(registry.snapshot("startup").is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn failed_stop_keeps_the_live_guard_and_an_unrelated_process_survives() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let other_drops = Arc::new(AtomicUsize::new(0));
        let mut child = spawn(&registry, "target", &drops);
        let mut unrelated = spawn(&registry, "unrelated", &other_drops);
        let pid = child.id();
        registry.faults.kill.store(true, Ordering::Release);
        assert_eq!(child.stop().await.unwrap_err().code, "process_stop");
        assert_eq!(child.id(), pid);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert_eq!(
            registry.snapshot("target").unwrap().phase,
            Phase::StopFailed
        );
        assert!(child.try_wait().unwrap().is_none());
        assert!(unrelated.try_wait().unwrap().is_none());
        assert_eq!(
            registry.retry_cleanup("target").await.unwrap_err().code,
            "process_guard_owned"
        );
        registry.faults.kill.store(false, Ordering::Release);
        child.stop().await.unwrap();
        drop(child);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(unrelated.try_wait().unwrap().is_none());
        assert_eq!(other_drops.load(Ordering::SeqCst), 0);
        unrelated.stop().await.unwrap();
        drop(unrelated);
        assert_eq!(other_drops.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn failed_background_cleanup_retains_charge_and_successful_retry_releases_it() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let child = spawn(&registry, "cancelled", &drops);
        let pid = child.id();
        let generation = child.generation();
        registry.faults.kill.store(true, Ordering::Release);
        drop(child);
        registry.faults.background_finished.notified().await;
        phase(
            &registry,
            "cancelled",
            Phase::CleanupPending,
            Some("process_stop"),
        )
        .await;
        let observed = registry.snapshot("cancelled").unwrap();
        assert_eq!((observed.pid, observed.generation), (pid, generation));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        registry.faults.kill.store(false, Ordering::Release);
        registry.retry_cleanup("cancelled").await.unwrap();
        assert!(registry.snapshot("cancelled").is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn cancelling_a_cleanup_attempt_restores_the_exact_generation_and_charge() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let child = spawn(&registry, "cancelled", &drops);
        let generation = child.generation();
        let pid = child.id();
        registry.faults.kill.store(true, Ordering::Release);
        drop(child);
        registry.faults.background_finished.notified().await;
        phase(
            &registry,
            "cancelled",
            Phase::CleanupPending,
            Some("process_stop"),
        )
        .await;
        registry.faults.kill.store(false, Ordering::Release);
        registry.faults.pause.store(true, Ordering::Release);
        let cleanup_registry = registry.clone();
        let cleanup =
            tokio::spawn(async move { cleanup_registry.retry_cleanup("cancelled").await });
        registry.faults.entered.notified().await;
        assert_eq!(
            registry.snapshot("cancelled").unwrap().phase,
            Phase::Cleaning
        );
        assert_eq!(
            registry.retry_cleanup("cancelled").await.unwrap_err().code,
            "process_cleanup_in_progress"
        );
        cleanup.abort();
        assert!(cleanup.await.unwrap_err().is_cancelled());
        let observed = registry.snapshot("cancelled").unwrap();
        assert_eq!(
            (observed.generation, observed.pid, observed.phase),
            (generation, pid, Phase::CleanupPending)
        );
        assert_eq!(observed.last_error, Some("process_cleanup_interrupted"));
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        registry.faults.pause.store(false, Ordering::Release);
        registry.faults.resume.notify_one();
        registry.retry_cleanup("cancelled").await.unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert!(registry.snapshot("cancelled").is_none());
    }
    #[tokio::test]
    async fn cancelling_live_stop_keeps_a_retryable_guard_and_resources() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut child = spawn(&registry, "live", &drops);
        registry.faults.pause.store(true, Ordering::Release);
        let mut attempt = Box::pin(child.stop());
        assert!(attempt.as_mut().now_or_never().is_none());
        assert_eq!(registry.snapshot("live").unwrap().phase, Phase::Stopping);
        drop(attempt);
        assert_eq!(registry.snapshot("live").unwrap().phase, Phase::StopFailed);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        assert!(child.try_wait().unwrap().is_none());
        registry.faults.pause.store(false, Ordering::Release);
        child.stop().await.unwrap();
        drop(child);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn wait_errors_never_publish_exit_or_release_a_resource_claim() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let mut child = spawn(&registry, "wait", &drops);
        registry.faults.wait.store(true, Ordering::Release);
        assert!(child.try_wait().is_err());
        assert_eq!(registry.snapshot("wait").unwrap().phase, Phase::StopFailed);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(child);
        phase(
            &registry,
            "wait",
            Phase::CleanupPending,
            Some("process_wait"),
        )
        .await;
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        registry.faults.wait.store(false, Ordering::Release);
        registry.retry_cleanup("wait").await.unwrap();
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn old_generation_confirmation_cannot_clear_or_signal_a_newer_child() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let first_drops = Arc::new(AtomicUsize::new(0));
        let next_drops = Arc::new(AtomicUsize::new(0));
        let mut old = spawn(&registry, "same", &first_drops);
        let generation = old.generation();
        old.stop().await.unwrap();
        let mut next = spawn(&registry, "same", &next_drops);
        assert!(next.generation() > generation);
        registry.confirmed("same", generation);
        drop(old);
        assert_eq!(
            registry.snapshot("same").unwrap().generation,
            next.generation()
        );
        assert!(next.try_wait().unwrap().is_none());
        assert_eq!(first_drops.load(Ordering::SeqCst), 1);
        assert_eq!(next_drops.load(Ordering::SeqCst), 0);
        next.stop().await.unwrap();
        drop(next);
        assert_eq!(next_drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn spawn_failure_rolls_back_the_starting_generation_and_new_resource_claim() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let path = std::env::current_exe()
            .unwrap()
            .with_file_name("juiced-nonexistent-process-fixture");
        assert!(!path.exists());
        assert_eq!(
            registry
                .spawn(
                    Arc::from("bad"),
                    &mut Command::new(path),
                    probe(&registry, &drops)
                )
                .unwrap_err()
                .code,
            "process_spawn"
        );
        assert!(registry.snapshot("bad").is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn runtime_teardown_restores_an_inflight_cleanup_without_releasing_resources() {
        let registry = Registry::new(DEFAULT_STOP_TIMEOUT);
        let drops = Arc::new(AtomicUsize::new(0));
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let child = spawn(&registry, "teardown", &drops);
            registry.faults.kill.store(true, Ordering::Release);
            drop(child);
            registry.faults.background_finished.notified().await;
            phase(
                &registry,
                "teardown",
                Phase::CleanupPending,
                Some("process_stop"),
            )
            .await;
            registry.faults.kill.store(false, Ordering::Release);
            registry.faults.pause.store(true, Ordering::Release);
            let cleanup_registry = registry.clone();
            tokio::spawn(async move {
                let _ = cleanup_registry.retry_cleanup("teardown").await;
            });
            registry.faults.entered.notified().await;
            assert_eq!(
                registry.snapshot("teardown").unwrap().phase,
                Phase::Cleaning
            );
        });
        drop(runtime);
        assert_eq!(
            registry.snapshot("teardown").unwrap().phase,
            Phase::CleanupPending
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        registry.faults.pause.store(false, Ordering::Release);
        registry.faults.resume.notify_one();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime
            .block_on(registry.retry_cleanup("teardown"))
            .unwrap();
        assert!(registry.snapshot("teardown").is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
    #[tokio::test]
    async fn a_kill_request_without_exit_acknowledgement_times_out_and_retains_charge() {
        let registry = Registry::new(Duration::from_millis(2));
        let drops = Arc::new(AtomicUsize::new(0));
        let mut child = spawn(&registry, "timeout", &drops);
        let generation = child.generation();
        registry.faults.withhold_ack.store(true, Ordering::Release);
        assert_eq!(child.stop().await.unwrap_err().code, "process_stop_timeout");
        assert_eq!(
            registry.snapshot("timeout").unwrap().phase,
            Phase::StopFailed
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(child);
        registry.faults.background_finished.notified().await;
        let observed = registry.snapshot("timeout").unwrap();
        assert_eq!(
            (observed.generation, observed.phase, observed.last_error),
            (
                generation,
                Phase::CleanupPending,
                Some("process_stop_timeout")
            )
        );
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        registry.faults.withhold_ack.store(false, Ordering::Release);
        registry.retry_cleanup("timeout").await.unwrap();
        assert!(registry.snapshot("timeout").is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}
