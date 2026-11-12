use super::*;
use std::ops::Bound;

pub const DEFAULT_PAGE_SIZE: usize = 128;

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PageRequest {
    pub limit: Option<usize>,
    pub after: Option<String>,
    pub expected_generation: Option<u64>,
    pub expected_view: Option<String>,
    pub orphan_after: Option<String>,
}

impl PageRequest {
    pub fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_PAGE_SIZE)
    }

    pub(crate) fn validate(&self, maximum: usize, inspection: bool) -> Result<(), CacheError> {
        if self.limit() == 0 || self.limit() > maximum {
            return Err(CacheError::Invalid(
                "Use a positive page limit within the store entry bound",
            ));
        }
        if let Some(after) = &self.after {
            validate_id(after)?;
        }
        if (self.after.is_some() || self.orphan_after.is_some())
            && (self.expected_generation.is_none() || self.expected_view.is_none())
        {
            return Err(CacheError::Invalid(
                "A continuation requires its store generation and view token",
            ));
        }
        if self
            .expected_view
            .as_deref()
            .is_some_and(|view| !digest_valid(view))
        {
            return Err(CacheError::Invalid("A view token must be an exact digest"));
        }
        if self
            .orphan_after
            .as_ref()
            .is_some_and(|name| !inspection || name.is_empty() || name.len() > 4096)
        {
            return Err(CacheError::Invalid(
                "Only inspection accepts a bounded orphan-name continuation",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PageInfo {
    pub view: String,
    pub limit: usize,
    pub next_after: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct OrphanPageInfo {
    pub limit: usize,
    pub next_after: Option<String>,
    pub observed_count: usize,
    pub scan_complete: bool,
}

fn view_token(runtime: &Runtime) -> String {
    let mut hash = Sha256::new();
    hash.update(b"juiced-cache-page-v1\0");
    hash.update(runtime.manager_owner.as_bytes());
    hash.update([u8::from(runtime.manifest.store_id.is_some())]);
    if let Some(id) = &runtime.manifest.store_id {
        hash.update(id.as_bytes());
    }
    hash.update(runtime.manifest.generation.to_le_bytes());
    hash.update((runtime.manifest.entries.len() as u64).to_le_bytes());
    hash.update((runtime.manifest.reader_intents.len() as u64).to_le_bytes());
    format!("{:x}", hash.finalize())
}

pub(super) fn rows(
    runtime: &Runtime,
    namespace: Option<(&str, &str)>,
    request: &PageRequest,
) -> Result<(Vec<SnapshotMetadata>, PageInfo), CacheError> {
    if request
        .expected_generation
        .is_some_and(|generation| generation != runtime.manifest.generation)
    {
        return Err(CacheError::Conflict);
    }
    let view = view_token(runtime);
    if request
        .expected_view
        .as_ref()
        .is_some_and(|expected| *expected != view)
    {
        return Err(CacheError::Conflict);
    }
    let visible = |record: &Record| {
        namespace.is_none_or(|(principal, epoch)| {
            record.identity.principal == principal && record.identity.credential_epoch == epoch
        })
    };
    if let Some(after) = &request.after
        && !runtime
            .manifest
            .entries
            .get(after)
            .is_some_and(|record| visible(record))
    {
        return Err(CacheError::NotFound);
    }
    let start = request
        .after
        .as_deref()
        .map_or(Bound::Unbounded, Bound::Excluded);
    let mut entries = runtime
        .manifest
        .entries
        .range::<str, _>((start, Bound::Unbounded))
        .filter(|(_, record)| visible(record));
    let mut rows = entries
        .by_ref()
        .take(request.limit())
        .map(|(id, record)| base_metadata(id, record, runtime))
        .collect::<Vec<_>>();
    let next_after = entries
        .next()
        .and_then(|_| rows.last().map(|row| row.id.clone()));
    for (id, intent) in &runtime.manifest.reader_intents {
        if let Ok(index) = rows.binary_search_by(|row| row.id.as_str().cmp(&intent.snapshot)) {
            rows[index]
                .retained_readers
                .push(reader::project(runtime, id, intent));
        }
    }
    Ok((
        rows,
        PageInfo {
            view,
            limit: request.limit(),
            next_after,
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::tests::{fixture, identity, saved};

    #[test]
    fn pages_preserve_namespace_order_and_refuse_stale_or_foreign_continuations_without_effects() {
        let (directory, store) = fixture();
        let mut expected = (0..3).map(|_| saved(&store).id).collect::<Vec<_>>();
        expected.sort();
        let mut foreign = identity();
        foreign.credential_epoch = "foreign-epoch".into();
        let pending = store.begin(foreign, store.generation(), 32).unwrap();
        fs::write(pending.path(), [1u8; 16]).unwrap();
        let foreign = store.publish(&pending, pending.revision()).unwrap();
        let namespace = identity();
        let mut request = PageRequest {
            limit: Some(1),
            ..Default::default()
        };
        let generation = store.generation();
        let mut observed = Vec::new();
        loop {
            let (current, rows, page) = store
                .inventory_page(&namespace.principal, &namespace.credential_epoch, &request)
                .unwrap();
            assert_eq!(current, generation);
            assert_eq!(rows.len(), 1);
            observed.push(rows[0].id.clone());
            let Some(after) = page.next_after else {
                break;
            };
            request.after = Some(after);
            request.expected_generation = Some(generation);
            request.expected_view = Some(page.view);
        }
        assert_eq!(observed, expected);
        let manifest = directory.path().join("cache/snapshots.json");
        let before = fs::read(&manifest).unwrap();
        request.after = Some(foreign.id);
        assert!(matches!(
            store.inventory_page(&namespace.principal, &namespace.credential_epoch, &request),
            Err(CacheError::NotFound)
        ));
        assert_eq!(fs::read(&manifest).unwrap(), before);
        request.after = Some(expected[0].clone());
        saved(&store);
        let before = fs::read(&manifest).unwrap();
        assert!(matches!(
            store.inventory_page(&namespace.principal, &namespace.credential_epoch, &request),
            Err(CacheError::Conflict)
        ));
        assert_eq!(fs::read(&manifest).unwrap(), before);
        request.expected_generation = None;
        assert!(matches!(
            store.inventory_page(&namespace.principal, &namespace.credential_epoch, &request),
            Err(CacheError::Invalid(_))
        ));
    }

    #[test]
    fn paged_inspection_hashes_only_selected_snapshots_and_orders_bounded_orphan_names() {
        let directory = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(directory.path().join("cache")).unwrap();
        let limits = CacheLimits {
            maximum_hash_bytes: 32,
            ..Default::default()
        };
        let store = CacheStore::open_with_limits(lease, limits).unwrap();
        let mut expected = (0..3).map(|_| saved(&store).id).collect::<Vec<_>>();
        expected.sort();
        let mut request = PageRequest {
            limit: Some(1),
            ..Default::default()
        };
        let mut observed = Vec::new();
        loop {
            let page = store.inspect_page(&request).unwrap();
            assert_eq!(page.snapshots.len(), 1);
            assert_eq!(page.snapshots[0].checksum_valid, Some(true));
            observed.push(page.snapshots[0].snapshot.id.clone());
            let continuation = page.page.unwrap();
            let Some(after) = continuation.next_after else {
                break;
            };
            request.after = Some(after);
            request.expected_generation = Some(page.generation);
            request.expected_view = Some(continuation.view);
        }
        assert_eq!(observed, expected);
        let mut names = Vec::new();
        for index in [4, 1, 3, 0, 2] {
            let name = format!("kv-orphan-{index:04}.bin");
            fs::write(directory.path().join("cache").join(&name), [1u8]).unwrap();
            names.push(name);
        }
        names.sort();
        let mut request = PageRequest {
            limit: Some(2),
            ..Default::default()
        };
        let mut observed = Vec::new();
        loop {
            let page = store.inspect_page(&request).unwrap();
            assert!(page.orphan_files.len() <= 2);
            observed.extend(page.orphan_files);
            let orphans = page.orphan_page.unwrap();
            assert_eq!(orphans.observed_count, 5);
            assert!(orphans.scan_complete);
            let Some(after) = orphans.next_after else {
                break;
            };
            request.orphan_after = Some(after);
            request.expected_generation = Some(page.generation);
            request.expected_view = Some(page.page.unwrap().view);
        }
        assert_eq!(observed, names);
    }

    #[test]
    fn invalid_page_bounds_and_incomplete_orphan_scans_never_claim_an_empty_complete_inventory() {
        let (_directory, store) = fixture();
        let namespace = identity();
        for request in [
            PageRequest {
                limit: Some(0),
                ..Default::default()
            },
            PageRequest {
                limit: Some(16385),
                ..Default::default()
            },
            PageRequest {
                after: Some("../outside".into()),
                expected_generation: Some(0),
                ..Default::default()
            },
            PageRequest {
                orphan_after: Some("kv-observed.bin".into()),
                expected_generation: Some(0),
                ..Default::default()
            },
        ] {
            assert!(matches!(
                store.inventory_page(&namespace.principal, &namespace.credential_epoch, &request),
                Err(CacheError::Invalid(_))
            ));
        }
        let directory = tempfile::tempdir().unwrap();
        let lease = DirectoryLease::acquire(directory.path().join("cache")).unwrap();
        let store = CacheStore::open_with_limits(
            lease,
            CacheLimits {
                inspection_timeout: Duration::from_nanos(1),
                ..Default::default()
            },
        )
        .unwrap();
        fs::write(directory.path().join("cache/kv-observed.bin"), [1u8]).unwrap();
        let page = store.inspect_page(&PageRequest::default()).unwrap();
        assert!(!page.orphan_page.unwrap().scan_complete);
        assert!(page.issues.contains(&"inspection_limit"));
    }

    #[cfg(unix)]
    #[test]
    fn uncommitted_retained_reservation_invalidates_the_view_even_when_disk_generation_is_unchanged()
     {
        let (directory, store) = fixture();
        let namespace = identity();
        let (generation, _, page) = store
            .inventory_page(
                &namespace.principal,
                &namespace.credential_epoch,
                &PageRequest::default(),
            )
            .unwrap();
        let root = directory.path().join("cache");
        let held = directory.path().join("held-cache");
        let before = fs::read(root.join("snapshots.json")).unwrap();
        let error = store
            .begin_observed(
                namespace.clone(),
                generation,
                1024,
                || Ok("f".repeat(32)),
                |point, _| {
                    if point == BeginPoint::BeforeCommit {
                        fs::rename(&root, &held).unwrap();
                        fs::create_dir(&root).unwrap();
                    }
                },
            )
            .err()
            .unwrap();
        assert!(matches!(error, CacheError::Persistence(_)));
        fs::remove_dir(&root).unwrap();
        fs::rename(&held, &root).unwrap();
        assert_eq!(fs::read(root.join("snapshots.json")).unwrap(), before);
        assert_eq!(store.generation(), generation);
        let request = PageRequest {
            expected_generation: Some(generation),
            expected_view: Some(page.view),
            ..Default::default()
        };
        assert!(matches!(
            store.inventory_page(&namespace.principal, &namespace.credential_epoch, &request),
            Err(CacheError::Conflict)
        ));
        let (_, rows, _) = store
            .inventory_page(
                &namespace.principal,
                &namespace.credential_epoch,
                &PageRequest::default(),
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, SnapshotState::Uncertain);
        assert_eq!(rows[0].reserved_bytes, 1024);
    }

    #[test]
    fn blocked_paged_inspection_does_not_pin_unselected_record_allocations_after_forget() {
        let (_directory, store) = fixture();
        let mut ids = (0..3).map(|_| saved(&store).id).collect::<Vec<_>>();
        ids.sort();
        let target = ids.last().unwrap();
        let weak = Arc::downgrade(
            store
                .runtime
                .lock()
                .unwrap()
                .manifest
                .entries
                .get(target)
                .unwrap(),
        );
        let generation = store.generation();
        let worker = store.clone();
        let (started, entered) = std::sync::mpsc::channel();
        let (resume, waiting) = std::sync::mpsc::channel();
        let thread = std::thread::spawn(move || {
            worker
                .inspect_observed(
                    Some(&PageRequest {
                        limit: Some(1),
                        ..Default::default()
                    }),
                    || {
                        started.send(()).unwrap();
                        waiting.recv_timeout(Duration::from_secs(5)).unwrap();
                    },
                )
                .unwrap()
        });
        entered.recv_timeout(Duration::from_secs(2)).unwrap();
        store.forget(target, &identity(), 2).unwrap();
        assert!(weak.upgrade().is_none());
        resume.send(()).unwrap();
        let inspection = thread.join().unwrap();
        assert_eq!(inspection.generation, generation);
        assert_eq!(inspection.snapshots.len(), 1);
        assert_eq!(inspection.snapshots[0].snapshot.id, ids[0]);
        assert_eq!(inspection.snapshots[0].checksum_valid, Some(true));
        assert_eq!(
            store
                .list(&identity().principal, &identity().credential_epoch)
                .unwrap()
                .len(),
            2
        );
    }
}
