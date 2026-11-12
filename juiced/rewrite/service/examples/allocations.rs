use juiced_service::{
    http::request_model,
    storage::{DirectoryLease, LiveRoots},
};
use serde_json::json;
#[path = "support/allocation_counter.rs"]
mod allocation_counter;
use allocation_counter::{Counted, Measurement};
use std::hint::black_box;

#[global_allocator]
static ALLOCATOR: Counted = Counted;

fn measure(label: &str, repetitions: u64, mut action: impl FnMut()) -> serde_json::Value {
    for _ in 0..100 {
        action();
    }
    let (_, measured) = Measurement::run(|| {
        for _ in 0..repetitions {
            action();
        }
    });
    json!({"case":label,"repetitions":repetitions,"allocations_per_request":(measured.allocations + measured.reallocations) as f64/repetitions as f64,"allocated_bytes_per_request":measured.requested_bytes as f64/repetitions as f64,"elapsed_ns_per_request":measured.elapsed_ns as f64/repetitions as f64,"allocator":measured})
}

fn root_verification() -> serde_json::Value {
    let directory = tempfile::tempdir().unwrap();
    let config = DirectoryLease::acquire(directory.path().join("config")).unwrap();
    let artifacts = config.claim(config.root().join("artifacts")).unwrap();
    let cache = config.claim(config.root().join("cache")).unwrap();
    let scope = LiveRoots::new(vec![config.clone(), artifacts.clone(), cache.clone()]).unwrap();
    let mut pairs = Vec::with_capacity(5);
    for pair in 0..5 {
        let recursive = || {
            measure("recursive three-lease verification", 10000, || {
                for root in [&config, &artifacts, &cache] {
                    black_box(root.verify()).unwrap();
                }
            })
        };
        let flattened = || {
            measure("flattened scope verification", 10000, || {
                black_box(scope.verify()).unwrap();
            })
        };
        let (baseline, current) = if pair % 2 == 0 {
            (recursive(), flattened())
        } else {
            let current = flattened();
            (recursive(), current)
        };
        pairs.push(json!({"pair":pair + 1,"order":if pair % 2 == 0 { ["recursive","flattened"] } else { ["flattened","recursive"] },"recursive":baseline,"flattened":current}));
    }
    let median = |case: &str, metric: &str| {
        let mut values: Vec<_> = pairs
            .iter()
            .map(|pair| pair[case][metric].as_f64().unwrap())
            .collect();
        values.sort_by(f64::total_cmp);
        values[values.len() / 2]
    };
    let metrics = [
        "allocations_per_request",
        "allocated_bytes_per_request",
        "elapsed_ns_per_request",
    ];
    let summary = ["recursive", "flattened"]
        .into_iter()
        .map(|case| {
            let values: serde_json::Map<_, _> = metrics
                .iter()
                .map(|metric| ((*metric).to_owned(), json!(median(case, metric))))
                .collect();
            (case.to_owned(), serde_json::Value::Object(values))
        })
        .collect::<serde_json::Map<_, _>>();
    json!({"scope":"Synchronous successful storage verification microbenchmark in one process; construction, fixture setup and output serialization excluded","measurement_unit":"one complete verification of the same config/artifacts/cache authority; not an HTTP request","platform":std::env::consts::OS,"root_path_ancestors":config.root().ancestors().count() - 1,"warmups_per_case":100,"paired_runs":pairs,"median":summary})
}

fn main() {
    allocation_counter::self_check();
    let body = serde_json::to_vec(&json!({"model":"fixture.chat","messages":[{"role":"user","content":"x".repeat(32768)}],"temperature":0,"stream":false})).unwrap();
    let baseline = measure("R0 full JSON parse and reserialization", 10000, || {
        let value: serde_json::Value = serde_json::from_slice(black_box(&body)).unwrap();
        black_box(value["model"].as_str().unwrap());
        black_box(serde_json::to_vec(&value).unwrap());
    });
    let current = measure(
        "borrowed routing view and original Bytes forwarding",
        10000,
        || {
            black_box(request_model(black_box(&body)).unwrap());
        },
    );
    let borrowed = matches!(request_model(&body).unwrap(), std::borrow::Cow::Borrowed(_));
    println!(
        "{}",
        json!({"schema_version":2,"scope":"Routing JSON and retained storage verification microbenchmarks only; excludes network, worker and inference","allocator_scope":"Successful Rust GlobalAlloc calls, requested/released bytes and phase live/peak bytes; excludes libc allocation, allocator arenas, RSS and syscall counts. Single-threaded counters add measurement overhead","model_borrowed":borrowed,"body_bytes":body.len(),"results":[baseline,current],"storage_root_verification":root_verification()})
    );
}
