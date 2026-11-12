#[path = "support/allocation_counter.rs"]
mod allocation_counter;

use allocation_counter::{Counted, Measurement};
use juiced_service::{
    cache::{CacheStore, OperationControl, PageRequest, SnapshotIdentity, WriterRequest},
    storage::DirectoryLease,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    hint::black_box,
    io::{BufWriter, Write},
    sync::{Arc, atomic::AtomicBool},
    time::{Duration, Instant},
};

#[global_allocator]
static ALLOCATOR: Counted = Counted;

fn rss() -> Option<Value> {
    let text = fs::read_to_string("/proc/self/status").ok()?;
    let field = |name: &str| {
        text.lines().find_map(|line| {
            line.strip_prefix(name)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()?
                .checked_mul(1024)
        })
    };
    Some(json!({"current_bytes":field("VmRSS:"),"process_high_water_bytes":field("VmHWM:")}))
}

fn phase<T>(reports: &mut Vec<Value>, name: &str, action: impl FnOnce() -> T) -> T {
    let (result, measured) = Measurement::run(action);
    assert_eq!(measured.failed, 0);
    reports.push(json!({"phase":name,"allocator":measured,"rss_after_phase":rss()}));
    result
}

fn run(entries: usize, maximal: bool, trial: usize) -> Value {
    let temp = tempfile::tempdir().unwrap();
    let lease = DirectoryLease::acquire(temp.path().join("cache")).unwrap();
    let identity = SnapshotIdentity {
        execution_fingerprint: "a".repeat(64),
        principal: "p".repeat(if maximal { 160 } else { 64 }),
        credential_epoch: "e".repeat(if maximal { 160 } else { 64 }),
        protocol: "c".repeat(if maximal { 64 } else { 32 }),
        session_digest: "b".repeat(64),
    };
    identity.validate().unwrap();
    let payload = [17u8; 64];
    let hash = format!("{:x}", Sha256::digest(payload));
    let manifest = lease.root().join("snapshots.json");
    {
        let record = json!({"identity":identity,"revision":2,"state":"saved","reserved_bytes":64,"bytes":64,"sha256":hash});
        let mut output = BufWriter::new(File::create(&manifest).unwrap());
        output
            .write_all(b"{\"schema_version\":1,\"generation\":1,\"entries\":{")
            .unwrap();
        for index in 0..entries {
            if index > 0 {
                output.write_all(b",").unwrap();
            }
            write!(output, "\"{index:032x}\":").unwrap();
            serde_json::to_writer(&mut output, &record).unwrap();
        }
        output.write_all(b"}}").unwrap();
        output.flush().unwrap();
    }
    let manifest_bytes = fs::metadata(&manifest).unwrap().len();
    assert!(manifest_bytes < 16 * 1024 * 1024);
    let fixture_hash = format!("{:x}", Sha256::digest(fs::read(&manifest).unwrap()));
    let mut reports = Vec::with_capacity(20);
    let store = phase(&mut reports, "open_and_validate", || {
        CacheStore::open(lease.clone()).unwrap()
    });
    assert_eq!(store.generation(), 1);
    let rows = phase(&mut reports, "inventory_response_held", || {
        store
            .inventory(&identity.principal, &identity.credential_epoch)
            .unwrap()
    });
    assert_eq!(rows.1.len(), entries);
    let view = juiced_service::supervisor::CacheView {
        schema_version: 1,
        enabled: true,
        configuration_generation: 1,
        generation: rows.0,
        data: rows.1,
        page: juiced_service::cache::PageInfo {
            view: "a".repeat(64),
            limit: entries,
            next_after: None,
        },
        qualification: "unqualified",
        scope: "explicit native slot snapshots",
    };
    let tree_wire = phase(
        &mut reports,
        "inventory_via_json_tree_serialization",
        || serde_json::to_vec(&serde_json::to_value(&view).unwrap()).unwrap(),
    );
    let direct_wire = phase(&mut reports, "inventory_direct_typed_serialization", || {
        serde_json::to_vec(&view).unwrap()
    });
    assert_eq!(
        serde_json::from_slice::<Value>(&tree_wire).unwrap(),
        serde_json::from_slice::<Value>(&direct_wire).unwrap()
    );
    drop(tree_wire);
    drop(direct_wire);
    phase(&mut reports, "drop_inventory_response", || drop(view));
    let metadata_wire = |request: &PageRequest| {
        let (generation, data, page) = store
            .inventory_page(&identity.principal, &identity.credential_epoch, request)
            .unwrap();
        let view = juiced_service::supervisor::CacheView {
            schema_version: 1,
            enabled: true,
            configuration_generation: 1,
            generation,
            data,
            page,
            qualification: "unqualified",
            scope: "explicit native slot snapshots",
        };
        let wire = serde_json::to_vec(&view).unwrap();
        (view, wire)
    };
    let full = phase(
        &mut reports,
        "full_page_metadata_and_direct_wire_held",
        || {
            metadata_wire(&PageRequest {
                limit: Some(16384),
                ..Default::default()
            })
        },
    );
    assert_eq!(full.0.data.len(), entries);
    let full_wire_bytes = full.1.len();
    drop(full);
    let bounded = phase(
        &mut reports,
        "first_page_metadata_and_direct_wire_held",
        || metadata_wire(&PageRequest::default()),
    );
    assert_eq!(bounded.0.data.len(), entries.min(128));
    let first_page_wire_bytes = bounded.1.len();
    drop(bounded);
    let walked = phase(
        &mut reports,
        "walk_all_pages_drop_each_metadata_and_direct_wire",
        || {
            let mut request = PageRequest::default();
            let mut rows = 0;
            let mut pages = 0;
            let mut wire_bytes = 0;
            loop {
                let (view, wire) = metadata_wire(&request);
                assert_eq!(view.generation, 1);
                for row in &view.data {
                    assert_eq!(u128::from_str_radix(&row.id, 16).unwrap(), rows as u128);
                    rows += 1;
                }
                pages += 1;
                wire_bytes += wire.len();
                let Some(after) = view.page.next_after else {
                    break;
                };
                request.after = Some(after);
                request.expected_generation = Some(view.generation);
                request.expected_view = Some(view.page.view);
            }
            (rows, pages, wire_bytes)
        },
    );
    assert_eq!(walked.0, entries);
    let pending = phase(&mut reports, "begin_clone_and_durable_publication", || {
        store
            .begin(identity.clone(), store.generation(), 64)
            .unwrap()
    });
    fs::write(pending.path(), payload).unwrap();
    let saved = phase(
        &mut reports,
        "publish_clone_and_durable_publication",
        || store.publish(&pending, pending.revision()).unwrap(),
    );
    assert_eq!(saved.bytes, Some(64));
    let read = store.restore(pending.id(), &identity).unwrap();
    let control = OperationControl::new(
        Instant::now() + Duration::from_secs(30),
        Arc::new(AtomicBool::new(false)),
    );
    let bound = phase(
        &mut reports,
        "bind_reader_clone_and_durable_publication",
        || {
            store
                .bind_reader(
                    read,
                    WriterRequest {
                        model: "fixture.model".into(),
                        generation: 1,
                        actor: None,
                    },
                    &control,
                )
                .unwrap()
        },
    );
    let settlement = phase(
        &mut reports,
        "settle_reader_journal_and_durable_publication",
        || store.settle_reader(&bound, &control).unwrap(),
    );
    assert!(settlement.settled);
    drop(bound);
    assert_eq!(fs::read(pending.path()).unwrap(), payload);
    assert!(
        store
            .list(&identity.principal, &identity.credential_epoch)
            .unwrap()
            .iter()
            .all(|row| row.retained_readers.is_empty())
    );
    let inspection = phase(
        &mut reports,
        "inspect_response_held_missing_fixture_files",
        || store.inspect().unwrap(),
    );
    assert_eq!(inspection.snapshots.len(), entries + 1);
    assert_eq!(
        inspection
            .snapshots
            .iter()
            .filter(|row| row.checksum_valid == Some(true))
            .count(),
        1
    );
    drop(inspection);
    drop(pending);
    phase(&mut reports, "drop_store_resident_metadata", || drop(store));
    json!({"trial":trial,"entries":entries,"maximal_identity_strings":maximal,"initial_manifest_bytes":manifest_bytes,"fixture_sha256":fixture_hash,"page_wire_bytes":{"full":full_wire_bytes,"first":first_page_wire_bytes,"walk":walked.2},"walked_pages":walked.1,"phases":reports})
}

fn dense_json() -> Value {
    let body = format!("[{}0]", "0,".repeat((1024 * 1024 - 3) / 2));
    assert!(body.len() <= 1024 * 1024);
    let (value, measured) =
        Measurement::run(|| serde_json::from_slice::<Value>(black_box(body.as_bytes())).unwrap());
    assert_eq!(measured.failed, 0);
    let elements = value.as_array().unwrap().len();
    black_box(&value);
    let generic_rss = rss();
    drop(value);
    let (slots, slots_measurement) =
        Measurement::run(|| juiced_service::profiles::parse_slots(body.as_bytes()));
    assert!(slots.is_err());
    let props = format!(
        "{{\"total_slots\":1,\"unknown\":[{}0]}}",
        "0,".repeat((1024 * 1024 - 64) / 2)
    );
    assert!(props.len() <= 1024 * 1024);
    let (parsed_props, props_measurement) =
        Measurement::run(|| juiced_service::profiles::parse_props(props.as_bytes()).unwrap());
    assert_eq!(parsed_props.slots, Some(1));
    json!({"scope":"Generic JSON-tree baseline compared with current bounded observation parsing; numeric-array input is rejected as slots, while props accepts an unknown dense array. Direct parser calls, not HTTP tests","wire_bytes":body.len(),"elements":elements,"allocator":measured,"rss_after_phase":generic_rss,"bounded_slots_refusal":slots_measurement,"bounded_props_unknown_fields":{"wire_bytes":props.len(),"allocator":props_measurement}})
}

fn main() {
    allocation_counter::self_check();
    let mut runs = Vec::new();
    for trial in 1..=5 {
        let cases = if trial % 2 == 1 {
            [(16, false), (16000, false), (16380, true)]
        } else {
            [(16380, true), (16000, false), (16, false)]
        };
        for (entries, maximal) in cases {
            runs.push(run(entries, maximal, trial));
        }
    }
    println!(
        "{}",
        json!({"schema_version":1,"scope":"Single-threaded direct production CacheStore operations on synthetic manifests. One newly published snapshot has physical bytes; initial Saved rows intentionally have absent files, so inspection measures its missing-file path. No worker, inference, HTTP or model-peak qualification.","allocator_scope":"Successful Rust GlobalAlloc calls and requested-layout live bytes. Excludes libc, allocator arenas/fragmentation, stack, kernel and device memory. RSS is separate and process high-water includes fixture setup and earlier phases. Counters add measurement overhead.","result_lifetime":"Each phase retains its returned value until a later explicit drop. Peak is above that phase's quiescent Rust-live baseline; other phase results and reporting metadata can remain live.","platform":std::env::consts::OS,"runs":runs,"dense_json":dense_json()})
    );
}
