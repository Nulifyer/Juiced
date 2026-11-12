use super::*;
use crate::admission::{Admission, Settings as AdmissionSettings};
use futures_util::FutureExt;
use std::sync::{
    Arc, Barrier,
    atomic::{AtomicUsize, Ordering},
};

fn manager(capacity: Option<u64>, available: Option<u64>, headroom: u64) -> Arc<Admission> {
    let admission = Admission::new(AdmissionSettings {
        memory: Settings {
            mode: Mode::Protected,
            capacity_bytes: capacity,
            essential_headroom_bytes: headroom,
            maximum_sample_age_ms: 5_000,
        },
        ..Default::default()
    })
    .unwrap();
    admission.publish_sample(available, Instant::now());
    admission
}

#[test]
fn missing_zero_stale_future_and_overflow_never_manufacture_capacity() {
    let now = Instant::now();
    let settings = Settings {
        mode: Mode::Protected,
        capacity_bytes: Some(100),
        ..Default::default()
    };
    let mut ledger = Ledger::default();
    assert_eq!(
        ledger
            .check(&settings, 1, Priority::Essential, now)
            .unwrap_err()
            .code,
        "unknown_host_capacity"
    );
    ledger.publish(Some(0), now);
    assert_eq!(
        ledger
            .check(&settings, 1, Priority::Essential, now)
            .unwrap_err()
            .code,
        "host_memory_exhausted"
    );
    ledger.publish(None, now);
    assert_eq!(
        ledger
            .check(&settings, 1, Priority::Essential, now)
            .unwrap_err()
            .code,
        "unknown_host_capacity"
    );
    ledger.publish(Some(100), now);
    assert!(
        ledger
            .check(&settings, 100, Priority::Essential, now)
            .is_ok()
    );
    assert_eq!(
        ledger
            .check(
                &settings,
                1,
                Priority::Essential,
                now + Duration::from_millis(501)
            )
            .unwrap_err()
            .code,
        "unknown_host_capacity"
    );
    assert_eq!(
        ledger
            .check(
                &settings,
                1,
                Priority::Essential,
                now - Duration::from_millis(1)
            )
            .unwrap_err()
            .code,
        "unknown_host_capacity"
    );
    assert_eq!(
        ledger
            .check(&settings, u64::MAX, Priority::Essential, now)
            .unwrap_err()
            .code,
        "invalid_memory_claim"
    );
    let unknown = Settings {
        capacity_bytes: None,
        ..settings
    };
    assert_eq!(
        ledger
            .check(&unknown, 1, Priority::Essential, now)
            .unwrap_err()
            .code,
        "unknown_host_capacity"
    );
}

#[test]
fn materialized_residency_is_charged_once_against_sampled_host_headroom() {
    let admission = manager(Some(1_000), Some(500), 0);
    let mut load = admission.reserve_host(300, Priority::Interactive).unwrap();
    assert_eq!(admission.snapshot().memory.unwrap().outstanding_bytes, 300);
    load.materialize(200).unwrap();
    admission.publish_sample(Some(300), Instant::now());
    let request = admission.reserve_host(250, Priority::Interactive).unwrap();
    assert_eq!(
        admission
            .reserve_host(51, Priority::Interactive)
            .err()
            .unwrap()
            .code,
        "host_memory_exhausted"
    );
    let snapshot = admission.snapshot().memory.unwrap();
    assert_eq!(
        (
            snapshot.reserved_bytes,
            snapshot.resident_bytes,
            snapshot.outstanding_bytes
        ),
        (450, 200, 250)
    );
    drop(request);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 200);
    drop(load);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 0);
}

#[test]
fn essential_headroom_applies_to_all_claims_on_configured_and_sampled_capacity() {
    for (capacity, available) in [(100, 200), (200, 100)] {
        let admission = manager(Some(capacity), Some(available), 20);
        let essential = admission.reserve_host(70, Priority::Essential).unwrap();
        assert_eq!(
            admission
                .reserve_host(11, Priority::Interactive)
                .err()
                .unwrap()
                .code,
            "host_memory_exhausted"
        );
        let ordinary = admission.reserve_host(10, Priority::Interactive).unwrap();
        let remainder = admission.reserve_host(20, Priority::Essential).unwrap();
        assert_eq!(
            admission
                .reserve_host(1, Priority::Essential)
                .err()
                .unwrap()
                .code,
            "host_memory_exhausted"
        );
        drop((essential, ordinary, remainder));
        assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 0);
    }
}

#[test]
fn invalid_materialization_preserves_original_charge_and_can_be_retried_once() {
    let admission = manager(Some(100), Some(100), 0);
    let mut load = admission.reserve_host(80, Priority::Interactive).unwrap();
    for bytes in [0, 81] {
        assert_eq!(
            load.materialize(bytes).unwrap_err().code,
            "invalid_resident_transition"
        );
    }
    assert_eq!(admission.snapshot().memory.unwrap().outstanding_bytes, 80);
    load.materialize(60).unwrap();
    assert_eq!(
        load.materialize(50).unwrap_err().code,
        "invalid_resident_transition"
    );
    assert_eq!(admission.snapshot().memory.unwrap().resident_bytes, 60);
    drop(load);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 0);
}

#[tokio::test]
async fn memory_waiting_and_granted_cancellation_refund_memory_and_slots_together() {
    let admission = manager(Some(100), Some(100), 0);
    let pool = admission
        .pool("cpu-and-uma-share-host", Some(2), 0)
        .unwrap();
    let peak = admission.reserve_host(80, Priority::Interactive).unwrap();
    let mut waiting = Box::pin(pool.acquire_with_memory(Priority::Interactive, 30));
    assert!(waiting.as_mut().now_or_never().is_none());
    assert_eq!(pool.snapshot().active, 0);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 80);
    drop(waiting);
    assert_eq!(admission.snapshot().pending, 0);
    let mut granted = Box::pin(pool.acquire_with_memory(Priority::Interactive, 30));
    assert!(granted.as_mut().now_or_never().is_none());
    drop(peak);
    assert_eq!(pool.snapshot().active, 1);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 30);
    drop(granted);
    assert_eq!(pool.snapshot().active, 0);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 0);
    let slot = pool
        .acquire_with_memory(Priority::Interactive, 100)
        .await
        .unwrap();
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 100);
    drop(slot);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 0);
}

#[tokio::test]
async fn separate_execution_pools_cannot_duplicate_one_host_budget() {
    let admission = manager(Some(100), Some(100), 0);
    let cpu = admission.pool("cpu", Some(1), 0).unwrap();
    let uma = admission.pool("uma-gpu", Some(1), 0).unwrap();
    let cpu_claim = cpu
        .acquire_with_memory(Priority::Interactive, 70)
        .await
        .unwrap();
    let mut gpu_claim = Box::pin(uma.acquire_with_memory(Priority::Interactive, 40));
    assert!(gpu_claim.as_mut().now_or_never().is_none());
    assert_eq!(uma.snapshot().active, 0);
    drop(cpu_claim);
    let gpu_claim = gpu_claim.await.unwrap();
    assert_eq!(uma.snapshot().active, 1);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 40);
    drop(gpu_claim);
}

#[test]
fn concurrent_load_reservations_share_one_atomic_capacity_check() {
    let admission = manager(Some(100), Some(100), 0);
    let ready = Arc::new(Barrier::new(9));
    let held = Arc::new(Barrier::new(9));
    let accepted = Arc::new(AtomicUsize::new(0));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let (admission, ready, held, accepted) = (
            admission.clone(),
            ready.clone(),
            held.clone(),
            accepted.clone(),
        );
        threads.push(std::thread::spawn(move || {
            ready.wait();
            let claim = admission.reserve_host(30, Priority::Interactive);
            if claim.is_ok() {
                accepted.fetch_add(1, Ordering::SeqCst);
            }
            held.wait();
            drop(claim);
        }));
    }
    ready.wait();
    held.wait();
    for thread in threads {
        thread.join().unwrap();
    }
    assert_eq!(accepted.load(Ordering::SeqCst), 3);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 0);
}

#[test]
fn old_samples_cannot_replace_new_unknown_observations() {
    let admission = manager(Some(100), Some(100), 0);
    let now = Instant::now();
    admission.publish_sample(None, now);
    admission.publish_sample(Some(100), now - Duration::from_millis(1));
    assert_eq!(
        admission
            .reserve_host(1, Priority::Essential)
            .err()
            .unwrap()
            .code,
        "unknown_host_capacity"
    );
    admission.publish_sample(Some(100), now + Duration::from_secs(60));
    assert_eq!(
        admission
            .reserve_host(1, Priority::Essential)
            .err()
            .unwrap()
            .code,
        "unknown_host_capacity"
    );
}

#[test]
fn legacy_wire_and_capacity_are_unchanged_and_protected_calls_require_a_profile() {
    let admission = Admission::new(AdmissionSettings::default()).unwrap();
    let wire = serde_json::to_value(admission.snapshot()).unwrap();
    assert!(wire.get("memory").is_none());
    assert!(wire["settings"].get("memory").is_none());
    let protected = manager(Some(100), Some(100), 0);
    let pool = protected.pool("model", Some(1), 0).unwrap();
    assert_eq!(
        pool.try_acquire(Priority::Interactive).err().unwrap().code,
        "resource_profile_required"
    );
    assert_eq!(
        pool.try_acquire_with_memory(Priority::Interactive, 0)
            .err()
            .unwrap()
            .code,
        "invalid_memory_claim"
    );
    assert_eq!(pool.snapshot().active, 0);
}

#[test]
fn pause_and_shutdown_keep_existing_claims_and_allow_cleanup() {
    let admission = manager(Some(100), Some(100), 0);
    let mut peak = admission.reserve_host(80, Priority::Interactive).unwrap();
    admission.pause(true);
    assert_eq!(
        admission
            .reserve_host(1, Priority::Essential)
            .err()
            .unwrap()
            .code,
        "admission_paused"
    );
    peak.materialize(50).unwrap();
    admission.shutdown();
    assert_eq!(
        admission
            .reserve_host(1, Priority::Essential)
            .err()
            .unwrap()
            .code,
        "shutting_down"
    );
    assert_eq!(admission.snapshot().memory.unwrap().resident_bytes, 50);
    drop(peak);
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 0);
}

#[test]
fn residency_transition_requires_a_sample_that_started_after_materialization() {
    let admission = manager(Some(100), Some(100), 0);
    let sample_started = Instant::now();
    let mut load = admission.reserve_host(80, Priority::Interactive).unwrap();
    load.materialize(50).unwrap();
    admission.publish_sample(Some(100), sample_started);
    assert_eq!(
        admission
            .reserve_host(1, Priority::Interactive)
            .err()
            .unwrap()
            .code,
        "unknown_host_capacity"
    );
    admission.publish_sample(Some(50), Instant::now());
    let next = admission.reserve_host(50, Priority::Interactive).unwrap();
    assert_eq!(admission.snapshot().memory.unwrap().reserved_bytes, 100);
    drop((load, next));
}
