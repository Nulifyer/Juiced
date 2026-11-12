use serde::Serialize;
use std::{
    alloc::{GlobalAlloc, Layout, System},
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

pub struct Counted;

static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static REALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static DEALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static FAILED: AtomicU64 = AtomicU64::new(0);
static REQUESTED: AtomicU64 = AtomicU64::new(0);
static RELEASED: AtomicU64 = AtomicU64::new(0);
static LIVE: AtomicU64 = AtomicU64::new(0);
static PEAK: AtomicU64 = AtomicU64::new(0);

fn grew(bytes: u64) {
    let live = LIVE.fetch_add(bytes, Ordering::Relaxed) + bytes;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if pointer.is_null() {
            FAILED.fetch_add(1, Ordering::Relaxed);
        } else {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            REQUESTED.fetch_add(layout.size() as u64, Ordering::Relaxed);
            grew(layout.size() as u64);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
        DEALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        RELEASED.fetch_add(layout.size() as u64, Ordering::Relaxed);
        LIVE.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let resized = unsafe { System.realloc(pointer, layout, size) };
        if resized.is_null() {
            FAILED.fetch_add(1, Ordering::Relaxed);
        } else {
            REALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            REQUESTED.fetch_add(size as u64, Ordering::Relaxed);
            RELEASED.fetch_add(layout.size() as u64, Ordering::Relaxed);
            if size >= layout.size() {
                grew((size - layout.size()) as u64);
            } else {
                LIVE.fetch_sub((layout.size() - size) as u64, Ordering::Relaxed);
            }
        }
        resized
    }
}

#[derive(Clone, Copy)]
struct Counters {
    allocations: u64,
    reallocations: u64,
    deallocations: u64,
    failed: u64,
    requested: u64,
    released: u64,
    live: u64,
}

impl Counters {
    fn read() -> Self {
        Self {
            allocations: ALLOCATIONS.load(Ordering::Relaxed),
            reallocations: REALLOCATIONS.load(Ordering::Relaxed),
            deallocations: DEALLOCATIONS.load(Ordering::Relaxed),
            failed: FAILED.load(Ordering::Relaxed),
            requested: REQUESTED.load(Ordering::Relaxed),
            released: RELEASED.load(Ordering::Relaxed),
            live: LIVE.load(Ordering::Relaxed),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Measurement {
    pub allocations: u64,
    pub reallocations: u64,
    pub deallocations: u64,
    pub failed: u64,
    pub requested_bytes: u64,
    pub released_bytes: u64,
    pub baseline_live_bytes: u64,
    pub final_live_bytes: u64,
    pub peak_extra_live_bytes: u64,
    pub elapsed_ns: u128,
}

impl Measurement {
    // Phases run on one thread without overlapping measurements or background work.
    pub fn run<T>(action: impl FnOnce() -> T) -> (T, Self) {
        let before = Counters::read();
        PEAK.store(before.live, Ordering::Relaxed);
        let started = Instant::now();
        let result = action();
        let elapsed_ns = started.elapsed().as_nanos();
        let after = Counters::read();
        let peak = PEAK.load(Ordering::Relaxed);
        (
            result,
            Self {
                allocations: after.allocations - before.allocations,
                reallocations: after.reallocations - before.reallocations,
                deallocations: after.deallocations - before.deallocations,
                failed: after.failed - before.failed,
                requested_bytes: after.requested - before.requested,
                released_bytes: after.released - before.released,
                baseline_live_bytes: before.live,
                final_live_bytes: after.live,
                peak_extra_live_bytes: peak.saturating_sub(before.live),
                elapsed_ns,
            },
        )
    }
}

pub fn self_check() {
    let (_, measured) = Measurement::run(|| unsafe {
        let initial = Layout::from_size_align(128, 8).unwrap();
        let pointer = std::hint::black_box(Counted.alloc(initial));
        assert!(!pointer.is_null());
        let pointer = std::hint::black_box(Counted.realloc(pointer, initial, 512));
        assert!(!pointer.is_null());
        let enlarged = Layout::from_size_align(512, 8).unwrap();
        let pointer = std::hint::black_box(Counted.realloc(pointer, enlarged, 64));
        assert!(!pointer.is_null());
        Counted.dealloc(pointer, Layout::from_size_align(64, 8).unwrap());
    });
    assert_eq!(measured.allocations, 1);
    assert_eq!(measured.reallocations, 2);
    assert_eq!(measured.deallocations, 1);
    assert_eq!(measured.requested_bytes, 704);
    assert_eq!(measured.released_bytes, 704);
    assert_eq!(measured.final_live_bytes, measured.baseline_live_bytes);
    assert_eq!(measured.peak_extra_live_bytes, 512);
    assert_eq!(measured.failed, 0);
}
