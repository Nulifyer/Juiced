use super::PhysicalClaims;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

pub(super) struct UncertainClaims {
    claims: Mutex<Vec<PhysicalClaims>>,
    maximum: usize,
    overflowed: AtomicBool,
}

impl UncertainClaims {
    pub(super) fn new(maximum: u32) -> Self {
        assert!((1..=65536).contains(&maximum));
        Self {
            claims: Mutex::new(Vec::with_capacity(maximum as usize)),
            maximum: maximum as usize,
            overflowed: AtomicBool::new(false),
        }
    }

    pub(super) fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::Acquire)
    }

    fn retain(&self, mut physical: PhysicalClaims) {
        if !physical.uncertain {
            if let Some(worker) = physical.worker.upgrade() {
                worker.uncertain_requests.fetch_add(1, Ordering::AcqRel);
            }
            physical.uncertain = true;
        }
        let mut claims = self
            .claims
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if claims.len() < self.maximum {
            claims.push(physical);
        } else {
            self.overflowed.store(true, Ordering::Release);
            if let Some(worker) = physical.worker.upgrade() {
                worker
                    .physical_invariant_failed
                    .store(true, Ordering::Release);
            }
            // Each retained claim owns a physical slot, so the matching pool capacity
            // forbids overflow. A broken caller invariant must never refund live work.
            std::mem::forget(physical);
        }
    }
}

pub(super) struct ProtectedTransfer {
    generation: Arc<UncertainClaims>,
    physical: Option<PhysicalClaims>,
    dispatched: bool,
}

impl ProtectedTransfer {
    pub(super) fn new(generation: Arc<UncertainClaims>, physical: PhysicalClaims) -> Self {
        Self {
            generation,
            physical: Some(physical),
            dispatched: false,
        }
    }

    pub(super) fn dispatched(&mut self) {
        self.dispatched = true;
    }

    pub(super) fn settled(&mut self) {
        self.dispatched = false;
        drop(self.physical.take());
    }
}

impl Drop for ProtectedTransfer {
    fn drop(&mut self) {
        if let Some(physical) = self.physical.take() {
            if self.dispatched {
                self.generation.retain(physical);
            } else {
                drop(physical);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::{Manager, Worker, tests};
    use std::sync::atomic::Ordering;

    async fn claims(manager: &Arc<Manager>, worker: &Arc<Worker>) -> PhysicalClaims {
        tests::speech_claims(manager, worker)
            .await
            .0
            .lock()
            .unwrap()
            .take()
            .unwrap()
    }

    fn counts(manager: &Manager, worker: &Worker, active: u32, uncertain: u32) {
        assert_eq!(worker.active.load(Ordering::Acquire), active);
        assert_eq!(worker.uncertain_requests.load(Ordering::Acquire), uncertain);
        assert_eq!(worker.slots.snapshot().active, active);
        assert_eq!(manager.power.snapshot().active, active as usize);
        assert!(manager.processes.snapshots().is_empty());
    }

    #[tokio::test]
    async fn predispatch_drop_and_original_eof_refund_exactly_once_without_a_runtime_task() {
        let (_directory, manager) = tests::manager();
        let worker = manager.worker("one").unwrap();
        let generation = Arc::new(UncertainClaims::new(1));
        let physical = claims(&manager, &worker).await;
        counts(&manager, &worker, 1, 0);
        drop(ProtectedTransfer::new(generation.clone(), physical));
        counts(&manager, &worker, 0, 0);
        let mut transfer =
            ProtectedTransfer::new(generation.clone(), claims(&manager, &worker).await);
        transfer.dispatched();
        counts(&manager, &worker, 1, 0);
        transfer.settled();
        counts(&manager, &worker, 0, 0);
        transfer.settled();
        drop(transfer);
        drop(generation);
        counts(&manager, &worker, 0, 0);
    }

    #[tokio::test]
    async fn dispatched_drop_retains_slot_power_and_active_until_exact_generation_owner_drops() {
        let (_directory, manager) = tests::manager();
        let worker = manager.worker("one").unwrap();
        let generation = Arc::new(UncertainClaims::new(1));
        let (pointer, capacity) = {
            let claims = generation.claims.lock().unwrap();
            (claims.as_ptr(), claims.capacity())
        };
        let mut transfer =
            ProtectedTransfer::new(generation.clone(), claims(&manager, &worker).await);
        transfer.dispatched();
        drop(transfer);
        counts(&manager, &worker, 1, 1);
        assert!(
            worker
                .slots
                .try_acquire(crate::admission::Priority::Interactive)
                .is_err()
        );
        let retained = generation.claims.lock().unwrap();
        assert_eq!(retained.len(), 1);
        assert_eq!(retained.as_ptr(), pointer);
        assert_eq!(retained.capacity(), capacity);
        drop(retained);
        assert!(!generation.overflowed());
        drop(generation);
        counts(&manager, &worker, 0, 0);
    }

    #[tokio::test]
    async fn failure_response_headers_do_not_supply_an_original_eof_witness() {
        let (_directory, manager) = tests::manager();
        let worker = manager.worker("one").unwrap();
        for status in [200, 429, 500] {
            let generation = Arc::new(UncertainClaims::new(1));
            let mut transfer =
                ProtectedTransfer::new(generation.clone(), claims(&manager, &worker).await);
            transfer.dispatched();
            let response = axum::http::Response::builder()
                .status(status)
                .body("undrained response")
                .unwrap();
            assert_eq!(response.status().as_u16(), status);
            drop(response);
            drop(transfer);
            counts(&manager, &worker, 1, 1);
            drop(generation);
            counts(&manager, &worker, 0, 0);
        }
    }

    #[tokio::test]
    async fn generation_owners_never_refund_another_workers_claims() {
        let (_first_directory, first) = tests::manager();
        let (_second_directory, second) = tests::manager();
        let first_worker = first.worker("one").unwrap();
        let second_worker = second.worker("one").unwrap();
        let first_generation = Arc::new(UncertainClaims::new(1));
        let second_generation = Arc::new(UncertainClaims::new(1));
        let mut first_transfer = ProtectedTransfer::new(
            first_generation.clone(),
            claims(&first, &first_worker).await,
        );
        let mut second_transfer = ProtectedTransfer::new(
            second_generation.clone(),
            claims(&second, &second_worker).await,
        );
        first_transfer.dispatched();
        second_transfer.dispatched();
        drop(first_transfer);
        drop(second_transfer);
        counts(&first, &first_worker, 1, 1);
        counts(&second, &second_worker, 1, 1);
        drop(first_generation);
        counts(&first, &first_worker, 0, 0);
        counts(&second, &second_worker, 1, 1);
        drop(second_generation);
        counts(&second, &second_worker, 0, 0);
    }

    #[tokio::test]
    async fn impossible_capacity_breach_sets_diagnostic_and_never_credits_unowned_work() {
        let (_first_directory, first) = tests::manager();
        let (_second_directory, second) = tests::manager();
        let first_worker = first.worker("one").unwrap();
        let second_worker = second.worker("one").unwrap();
        let generation = Arc::new(UncertainClaims::new(1));
        let mut first_transfer =
            ProtectedTransfer::new(generation.clone(), claims(&first, &first_worker).await);
        let mut wrong_generation =
            ProtectedTransfer::new(generation.clone(), claims(&second, &second_worker).await);
        first_transfer.dispatched();
        wrong_generation.dispatched();
        drop(first_transfer);
        drop(wrong_generation);
        assert!(generation.overflowed());
        counts(&first, &first_worker, 1, 1);
        counts(&second, &second_worker, 1, 1);
        assert_eq!(generation.claims.lock().unwrap().len(), 1);
        drop(generation);
        counts(&first, &first_worker, 0, 0);
        counts(&second, &second_worker, 1, 1);
        assert!(
            second_worker
                .physical_invariant_failed
                .load(Ordering::Acquire)
        );
        let report = second.shutdown().await;
        assert!(!report.physical_claims_settled);
        assert!(!report.is_settled());
    }
}
