use super::Manager;
use crate::ApiError;

impl Manager {
    /// The worker's address while it runs, without claiming a request slot or waiting on a
    /// start, so engine probes never queue behind a generation.
    pub fn running_base(&self, id: &str) -> Result<Option<String>, ApiError> {
        let (worker, _) = self.resolve_model(id)?;
        let Ok(mut state) = worker.state.try_lock() else {
            return Ok(None);
        };
        Self::reap(&mut state);
        Ok(state.running.as_ref().map(|run| run.base.clone()))
    }
}
