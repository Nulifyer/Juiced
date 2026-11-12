use crate::admission::{Admission, Error, MemoryReservation, Priority};
use serde::Serialize;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

#[derive(Clone, Copy, Serialize)]
pub(crate) struct Bounds {
    pub metadata_bytes: u64,
    pub bootstrap_bytes: u64,
    pub import_bytes: u64,
    pub response_work_bytes: u64,
    pub response_bytes: u64,
    pub binding_work_bytes: u64,
}
impl Bounds {
    pub(crate) const fn selected() -> Self {
        Self {
            metadata_bytes: 32 * 1024 * 1024,
            bootstrap_bytes: 48 * 1024 * 1024,
            import_bytes: 64 * 1024 * 1024,
            response_work_bytes: 8 * 1024 * 1024,
            response_bytes: 4 * 1024 * 1024,
            binding_work_bytes: 24 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Usage {
    metadata: u64,
    bindings: u64,
    binding_count: usize,
    work: u64,
    active: usize,
    closing: bool,
}

pub(crate) struct Controller {
    admission: Arc<Admission>,
    usage: Mutex<Usage>,
    settled: Notify,
}

#[derive(Clone, Copy)]
enum Kind {
    Work,
    Metadata,
    Binding,
}

pub(crate) struct Claim {
    owner: Arc<Controller>,
    kind: Kind,
    bytes: u64,
    reservation: Option<MemoryReservation>,
}

impl Controller {
    pub(crate) fn new(admission: Arc<Admission>) -> Arc<Self> {
        Arc::new(Self {
            admission,
            usage: Mutex::new(Usage::default()),
            settled: Notify::new(),
        })
    }

    fn claim(self: &Arc<Self>, bytes: u64, control: bool) -> Result<Claim, Error> {
        let reservation = if self.admission.memory_protected() {
            Some(if control {
                self.admission
                    .reserve_control_host(bytes, Priority::Interactive)?
            } else {
                self.admission.reserve_host(bytes, Priority::Interactive)?
            })
        } else {
            None
        };
        let charged = if reservation.is_some() { bytes } else { 0 };
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        if usage.closing {
            return Err(Error {
                status: 503,
                code: "runtime_controller_stopping",
                message: "Runtime controller work is closed while the service is stopping.",
            });
        }
        usage.active += 1;
        usage.work += charged;
        Ok(Claim {
            owner: self.clone(),
            kind: Kind::Work,
            bytes: charged,
            reservation,
        })
    }

    pub(crate) fn bootstrap(self: &Arc<Self>) -> Result<Claim, Error> {
        self.claim(Bounds::selected().bootstrap_bytes, true)
    }

    pub(crate) fn import(self: &Arc<Self>) -> Result<Claim, Error> {
        self.claim(Bounds::selected().import_bytes, false)
    }

    pub(crate) fn response(self: &Arc<Self>) -> Result<Claim, Error> {
        self.claim(Bounds::selected().response_work_bytes, true)
    }

    pub(crate) fn binding(self: &Arc<Self>, retained: u64) -> Result<Claim, Error> {
        let peak = retained
            .checked_add(Bounds::selected().binding_work_bytes)
            .ok_or(Error {
                status: 503,
                code: "runtime_memory_limits",
                message: "Runtime binding metadata exceeds its supported memory bound.",
            })?;
        self.claim(peak, true)
    }

    pub(crate) fn projection(&self) -> serde_json::Value {
        let usage = *self.usage.lock().unwrap_or_else(|error| error.into_inner());
        serde_json::json!({"state":if self.admission.memory_protected() {"accounted"} else {"legacy"},
            "scope":"Selected runtime controller layout ceilings, not process RSS or worker peak calibration",
            "bounds":Bounds::selected(),"metadata_bytes":usage.metadata,"binding_bytes":usage.bindings,
            "binding_count":usage.binding_count,"active_operations":usage.active,"scratch_bytes":usage.work,
            "reserved_bytes":usage.metadata+usage.bindings+usage.work,"outstanding_bytes":usage.metadata+usage.bindings+usage.work})
    }

    pub(crate) async fn drain(&self, deadline: tokio::time::Instant) -> bool {
        self.usage
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .closing = true;
        loop {
            let settled = self.settled.notified();
            tokio::pin!(settled);
            settled.as_mut().enable();
            if self
                .usage
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .active
                == 0
            {
                return true;
            }
            if tokio::time::timeout_at(deadline, settled).await.is_err() {
                return self
                    .usage
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .active
                    == 0;
            }
        }
    }
}

fn remove(usage: &mut Usage, kind: Kind, bytes: u64) {
    match kind {
        Kind::Work => {
            usage.active -= 1;
            usage.work -= bytes;
        }
        Kind::Metadata => usage.metadata -= bytes,
        Kind::Binding => {
            usage.binding_count -= 1;
            usage.bindings -= bytes;
        }
    }
}

impl Claim {
    pub(crate) fn controller(&self) -> &Arc<Controller> {
        &self.owner
    }

    fn retain(mut self, kind: Kind, bytes: u64) -> Result<Self, Error> {
        if let Some(reservation) = &mut self.reservation {
            reservation.shrink_outstanding(bytes)?;
        }
        let bytes = if self.reservation.is_some() { bytes } else { 0 };
        {
            let mut usage = self
                .owner
                .usage
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            remove(&mut usage, self.kind, self.bytes);
            match kind {
                Kind::Metadata => usage.metadata += bytes,
                Kind::Binding => {
                    usage.binding_count += 1;
                    usage.bindings += bytes;
                }
                Kind::Work => {
                    usage.active += 1;
                    usage.work += bytes;
                }
            }
            self.kind = kind;
            self.bytes = bytes;
        }
        self.owner.settled.notify_waiters();
        Ok(self)
    }

    pub(crate) fn metadata(self) -> Result<Self, Error> {
        self.retain(Kind::Metadata, Bounds::selected().metadata_bytes)
    }

    pub(crate) fn binding(self, bytes: u64) -> Result<Self, Error> {
        self.retain(Kind::Binding, bytes)
    }

    pub(crate) fn response(self) -> Result<Self, Error> {
        self.retain(Kind::Work, Bounds::selected().response_work_bytes)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        let mut usage = self
            .owner
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        remove(&mut usage, self.kind, self.bytes);
        drop(usage);
        self.owner.settled.notify_waiters();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{MemoryMode, MemorySettings, Settings};

    fn controller(capacity: u64) -> (Arc<Admission>, Arc<Controller>) {
        let admission = Admission::new(Settings {
            memory: MemorySettings {
                mode: MemoryMode::Protected,
                capacity_bytes: Some(capacity),
                essential_headroom_bytes: 0,
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        admission.seed_host_sample_for_test(Some(1024 * 1024 * 1024));
        let control = Controller::new(admission.clone());
        (admission, control)
    }

    #[tokio::test]
    async fn persistent_metadata_and_bindings_stay_charged_without_blocking_actual_work_drain() {
        let (admission, control) = controller(256 * 1024 * 1024);
        let metadata = control.bootstrap().unwrap().metadata().unwrap();
        let binding = control.binding(65536).unwrap().binding(65536).unwrap();
        assert_eq!(control.projection()["active_operations"], 0);
        let body = control.response().unwrap();
        assert!(!control.drain(tokio::time::Instant::now()).await);
        let expected =
            Bounds::selected().metadata_bytes + 65536 + Bounds::selected().response_work_bytes;
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            expected
        );
        assert_eq!(control.projection()["reserved_bytes"], expected);
        drop(body);
        assert!(control.drain(tokio::time::Instant::now()).await);
        assert_eq!(
            control.response().err().unwrap().code,
            "runtime_controller_stopping"
        );
        drop(binding);
        assert_eq!(control.projection()["binding_count"], 0);
        drop(metadata);
        assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 0);
    }

    #[test]
    fn capacity_pause_and_unknown_refusals_never_create_unfunded_work() {
        let (admission, control) = controller(Bounds::selected().import_bytes - 1);
        assert_eq!(
            control.import().err().unwrap().code,
            "host_memory_exhausted"
        );
        assert_eq!(control.projection()["active_operations"], 0);
        admission.pause(true);
        assert_eq!(control.import().err().unwrap().code, "admission_paused");
        let response = control.response().unwrap();
        admission.seed_host_sample_for_test(None);
        assert_eq!(
            control.response().err().unwrap().code,
            "unknown_host_capacity"
        );
        drop(response);
        assert_eq!(admission.memory_snapshot().unwrap().outstanding_bytes, 0);
    }

    #[test]
    fn completed_import_reduces_its_claim_only_after_copy_work_and_keeps_response_owned() {
        let (admission, control) = controller(256 * 1024 * 1024);
        let work = control.import().unwrap();
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            Bounds::selected().import_bytes
        );
        let response = work.response().unwrap();
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            Bounds::selected().response_work_bytes
        );
        assert_eq!(control.projection()["active_operations"], 1);
        drop(response);
        assert_eq!(control.projection()["active_operations"], 0);
    }
}
