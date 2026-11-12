use super::CacheLimits;
use crate::admission::{Admission, MemoryReservation, Priority};
use serde::Serialize;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) struct Bounds {
    pub bootstrap_bytes: u64,
    pub metadata_bytes: u64,
    pub scratch_bytes: u64,
    pub state_scratch_bytes: u64,
    pub page_scratch_bytes: u64,
    pub response_bytes: u64,
}

impl Bounds {
    pub(crate) fn for_limits(limits: &CacheLimits) -> Result<Self, crate::admission::Error> {
        let overflow = || crate::admission::Error {
            status: 400,
            code: "cache_memory_limits",
            message: "Cache metadata limits exceed supported controller-memory bounds.",
        };
        let entries = u64::try_from(limits.maximum_entries).map_err(|_| overflow())?;
        let metadata_bytes = limits
            .maximum_manifest_bytes
            .checked_mul(2)
            .and_then(|bytes| entries.checked_mul(2048)?.checked_add(bytes))
            .and_then(|bytes| bytes.checked_add(65536))
            .ok_or_else(overflow)?;
        let bootstrap_bytes = limits
            .maximum_manifest_bytes
            .checked_mul(3)
            .and_then(|bytes| bytes.checked_add(metadata_bytes))
            .and_then(|bytes| bytes.checked_add(2 * 1024 * 1024))
            .ok_or_else(overflow)?;
        let scratch_bytes = entries
            .checked_mul(6144)
            .and_then(|bytes| bytes.checked_add(2 * 1024 * 1024))
            .ok_or_else(overflow)?;
        let state_scratch_bytes = entries
            .checked_mul(512)
            .and_then(|bytes| bytes.checked_add(14 * 1024 * 1024))
            .ok_or_else(overflow)?;
        let page_scratch_bytes = entries
            .checked_mul(384)
            .and_then(|bytes| bytes.checked_add(2 * 1024 * 1024))
            .ok_or_else(overflow)?;
        let response_bytes = limits
            .maximum_manifest_bytes
            .checked_mul(2)
            .ok_or_else(overflow)?;
        if [
            metadata_bytes,
            bootstrap_bytes,
            scratch_bytes,
            state_scratch_bytes,
            page_scratch_bytes,
            response_bytes,
        ]
        .into_iter()
        .any(|bytes| bytes == 0 || bytes > i64::MAX as u64)
        {
            return Err(overflow());
        }
        Ok(Self {
            bootstrap_bytes,
            metadata_bytes,
            scratch_bytes,
            state_scratch_bytes,
            page_scratch_bytes,
            response_bytes,
        })
    }
}

pub(crate) struct Bootstrap {
    reservation: MemoryReservation,
    admission: Arc<Admission>,
    bounds: Bounds,
}

impl Bootstrap {
    pub(crate) fn reserve(
        admission: &Arc<Admission>,
        limits: &CacheLimits,
    ) -> Result<Self, crate::admission::Error> {
        if !admission.memory_protected() {
            return Err(crate::admission::Error {
                status: 400,
                code: "cache_memory_mode",
                message: "Cache controller accounting requires the protected host ledger.",
            });
        }
        let bounds = Bounds::for_limits(limits)?;
        Ok(Self {
            reservation: admission.reserve_host(bounds.bootstrap_bytes, Priority::Essential)?,
            admission: admission.clone(),
            bounds,
        })
    }

    pub(crate) fn retain_metadata(mut self) -> Result<Arc<ControlMemory>, crate::admission::Error> {
        self.reservation
            .shrink_outstanding(self.bounds.metadata_bytes)?;
        Ok(Arc::new(ControlMemory {
            admission: self.admission,
            bounds: self.bounds,
            usage: Mutex::new((0, 0)),
            _metadata: self.reservation,
        }))
    }
}

pub(crate) struct ControlMemory {
    admission: Arc<Admission>,
    pub(crate) bounds: Bounds,
    usage: Mutex<(u64, u64)>,
    _metadata: MemoryReservation,
}

impl ControlMemory {
    pub(crate) fn reserve(
        self: &Arc<Self>,
        work: Work,
    ) -> Result<Scratch, crate::admission::Error> {
        let bytes = match work {
            Work::Bounded => self.bounds.state_scratch_bytes,
            Work::Metadata => self.bounds.scratch_bytes,
            Work::Page => self.bounds.page_scratch_bytes,
        };
        let reservation = self
            .admission
            .reserve_control_host(bytes, Priority::Interactive)?;
        let mut usage = self.usage.lock().unwrap_or_else(|error| error.into_inner());
        usage.0 += 1;
        usage.1 += bytes;
        Ok(Scratch {
            owner: self.clone(),
            bytes,
            _reservation: reservation,
        })
    }

    pub(crate) fn projection(&self) -> serde_json::Value {
        let (active, scratch) = *self.usage.lock().unwrap_or_else(|error| error.into_inner());
        serde_json::json!({"state":"accounted","scope":"cache controller metadata and bounded scratch; declared layout bounds, not process RSS","bounds":self.bounds,"metadata_bytes":self.bounds.metadata_bytes,"active_operations":active,"scratch_bytes":scratch,"reserved_bytes":self.bounds.metadata_bytes + scratch,"outstanding_bytes":self.bounds.metadata_bytes + scratch,"reservation_state":"outstanding-ceiling"})
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Work {
    Bounded,
    Metadata,
    Page,
}

pub(crate) struct Scratch {
    owner: Arc<ControlMemory>,
    bytes: u64,
    _reservation: MemoryReservation,
}

impl Scratch {
    pub(crate) fn response_limit(&self) -> u64 {
        self.owner.bounds.response_bytes
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let mut usage = self
            .owner
            .usage
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        usage.0 -= 1;
        usage.1 -= self.bytes;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::admission::{MemoryMode, MemorySettings, Settings};

    fn admission(capacity: Option<u64>) -> Arc<Admission> {
        let admission = Admission::new(Settings {
            memory: MemorySettings {
                mode: MemoryMode::Protected,
                capacity_bytes: capacity,
                ..Default::default()
            },
            ..Default::default()
        })
        .unwrap();
        admission.seed_host_sample_for_test(Some(1024 * 1024 * 1024));
        admission
    }

    #[test]
    fn bootstrap_and_scratch_share_the_host_pool_and_refund_only_after_last_owner() {
        let admission = admission(Some(1024 * 1024 * 1024));
        let limits = CacheLimits::default();
        let bounds = Bounds::for_limits(&limits).unwrap();
        let bootstrap = Bootstrap::reserve(&admission, &limits).unwrap();
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            bounds.bootstrap_bytes
        );
        let memory = bootstrap.retain_metadata().unwrap();
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            bounds.metadata_bytes
        );
        assert_eq!(admission.memory_snapshot().unwrap().resident_bytes, 0);
        admission.seed_host_sample_for_test(None);
        assert_eq!(
            memory.reserve(Work::Metadata).err().unwrap().code,
            "unknown_host_capacity"
        );
        admission.seed_host_sample_for_test(Some(1024 * 1024 * 1024));
        let scratch = Arc::new(memory.reserve(Work::Metadata).unwrap());
        let remaining = scratch.clone();
        drop(scratch);
        assert_eq!(memory.projection()["active_operations"], 1);
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            bounds.metadata_bytes + bounds.scratch_bytes
        );
        drop(memory);
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            bounds.metadata_bytes + bounds.scratch_bytes
        );
        drop(remaining);
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 0);
    }

    #[test]
    fn unknown_or_insufficient_capacity_refuses_bootstrap_without_reservation() {
        for capacity in [None, Some(0), Some(1)] {
            let admission = admission(capacity);
            assert!(Bootstrap::reserve(&admission, &CacheLimits::default()).is_err());
            assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 0);
        }
        let admission = admission(Some(1024 * 1024 * 1024));
        admission.seed_host_sample_for_test(None);
        assert!(Bootstrap::reserve(&admission, &CacheLimits::default()).is_err());
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 0);
    }

    #[test]
    fn different_work_classes_charge_their_own_scratch_on_the_same_pool() {
        let admission = admission(Some(1024 * 1024 * 1024));
        let memory = Bootstrap::reserve(&admission, &CacheLimits::default())
            .unwrap()
            .retain_metadata()
            .unwrap();
        admission.seed_host_sample_for_test(Some(1024 * 1024 * 1024));
        let state = memory.reserve(Work::Bounded).unwrap();
        let metadata = memory.reserve(Work::Metadata).unwrap();
        let total = memory.bounds.state_scratch_bytes + memory.bounds.scratch_bytes;
        assert_eq!(memory.projection()["scratch_bytes"], total);
        assert_eq!(memory.projection()["active_operations"], 2);
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            memory.bounds.metadata_bytes + total
        );
        drop(metadata);
        assert_eq!(
            memory.projection()["scratch_bytes"],
            memory.bounds.state_scratch_bytes
        );
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            memory.bounds.metadata_bytes + memory.bounds.state_scratch_bytes
        );
        drop(state);
        assert_eq!(memory.projection()["active_operations"], 0);
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            memory.bounds.metadata_bytes
        );
    }

    #[test]
    fn actual_store_and_snapshot_read_lease_retain_metadata_ceiling_until_last_drop() {
        let admission = admission(Some(1024 * 1024 * 1024));
        let directory = tempfile::tempdir().unwrap();
        let lease =
            crate::storage::DirectoryLease::acquire(directory.path().join("cache")).unwrap();
        let scope = crate::storage::LiveRoots::single(lease.clone()).unwrap();
        let limits = CacheLimits::default();
        let bootstrap = Bootstrap::reserve(&admission, &limits).unwrap();
        let store =
            crate::cache::CacheStore::open_accounted(lease, limits, scope, bootstrap).unwrap();
        let identity = crate::cache::SnapshotIdentity {
            execution_fingerprint: "a".repeat(64),
            principal: "fixture".into(),
            credential_epoch: "epoch".into(),
            protocol: "openai.completions".into(),
            session_digest: "b".repeat(64),
        };
        let pending = store
            .begin(identity.clone(), store.generation(), 64)
            .unwrap();
        std::fs::write(pending.path(), [1u8; 64]).unwrap();
        store.publish(&pending, pending.revision()).unwrap();
        let read = store.restore(pending.id(), &identity).unwrap();
        drop(pending);
        let resident = store.control_memory().unwrap().bounds.metadata_bytes;
        drop(store);
        assert_eq!(
            admission.memory_snapshot().unwrap().outstanding_bytes,
            resident
        );
        assert_eq!(std::fs::read(read.path()).unwrap(), [1u8; 64]);
        drop(read);
        assert_eq!(admission.memory_snapshot().unwrap().reserved_bytes, 0);
    }
}
