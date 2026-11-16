use super::{ModelView, Worker, unix_time_ms};
use std::{collections::HashMap, sync::Mutex, time::Instant};

/// How long each model took to become ready on its engine, kept in memory.
#[derive(Default)]
pub(super) struct LoadTimes {
    inner: Mutex<Inner>,
}

#[derive(Default)]
struct Inner {
    started: HashMap<String, u64>,
    last: HashMap<(String, String), u64>,
}

pub(super) struct Attempt<'a> {
    times: &'a LoadTimes,
    model: String,
    engine: String,
    started: Instant,
}

/// The engine a load ran on: the managed runtime package, else the configured command.
fn engine(worker: &Worker) -> String {
    let profile = &worker.spec.execution_profile;
    profile
        .runtime_package()
        .map_or_else(|| profile.command.clone(), str::to_owned)
}

impl LoadTimes {
    pub(super) fn begin(&self, worker: &Worker) -> Attempt<'_> {
        let model = worker.spec.id.clone();
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .started
            .insert(model.clone(), unix_time_ms());
        Attempt {
            times: self,
            model,
            engine: engine(worker),
            started: Instant::now(),
        }
    }

    pub(super) fn overlay(&self, worker: &Worker, view: &mut ModelView) {
        let inner = self.inner.lock().unwrap_or_else(|error| error.into_inner());
        view.loading_started_unix_ms = (view.state == "loading")
            .then(|| inner.started.get(&view.id).copied())
            .flatten();
        view.last_load_ms = inner.last.get(&(view.id.clone(), engine(worker))).copied();
    }
}

impl Attempt<'_> {
    pub(super) fn ready(self) {
        let elapsed = self.started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        self.times
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .last
            .insert((self.model.clone(), self.engine.clone()), elapsed);
    }
}

impl Drop for Attempt<'_> {
    fn drop(&mut self) {
        self.times
            .inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .started
            .remove(&self.model);
    }
}
