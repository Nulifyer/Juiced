use super::*;

pub(crate) struct Store {
    pub(super) owner: DirectoryLease,
    pub(super) scope: Arc<LiveRoots>,
    current: std::sync::RwLock<Arc<Head>>,
    publishing: std::sync::atomic::AtomicBool,
    failed: std::sync::atomic::AtomicBool,
    importing: std::sync::Mutex<()>,
    _memory: Option<crate::runtime_packages::memory::Claim>,
}
struct Head {
    observed: Option<crate::storage::MetadataObservation>,
    manifest: Manifest,
    uncertain: Option<(Error, Manifest, Arc<Head>)>,
}

struct Publication<'a> {
    store: &'a Store,
    known: bool,
}
impl Drop for Publication<'_> {
    fn drop(&mut self) {
        if !self.known {
            self.store.failed.store(true, Ordering::Release);
        }
        self.store.publishing.store(false, Ordering::Release);
    }
}

pub(crate) struct Lease {
    store: Arc<Store>,
    id: String,
    payload: RuntimePayload,
    bundle: BundleSpec,
    _binding: BindingLease,
    _memory: Option<crate::runtime_packages::memory::Claim>,
}
struct BindingLease(Arc<Record>);
impl BindingLease {
    fn acquire(record: Arc<Record>) -> Result<Self> {
        record
            .binding_leases
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                count.checked_add(1)
            })
            .map_err(|_| Error::new("package_lease_limit"))?;
        Ok(Self(record))
    }
}
impl Drop for BindingLease {
    fn drop(&mut self) {
        let previous = self.0.binding_leases.fetch_sub(1, Ordering::AcqRel);
        debug_assert!(previous > 0);
    }
}
impl fmt::Debug for Lease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RuntimePackageLease")
            .finish_non_exhaustive()
    }
}
impl Store {
    #[cfg(test)]
    pub(crate) fn open_optional(
        root: DirectoryLease,
        existing: Arc<LiveRoots>,
    ) -> Result<Option<Arc<Self>>> {
        Self::open(root, existing, false, None)
    }
    pub(crate) fn open_optional_accounted(
        root: DirectoryLease,
        existing: Arc<LiveRoots>,
        control: Arc<crate::runtime_packages::memory::Controller>,
    ) -> Result<Option<Arc<Self>>> {
        Self::open(root, existing, false, Some(control))
    }
    pub(crate) fn open_for_import(
        root: DirectoryLease,
        existing: Arc<LiveRoots>,
    ) -> Result<Arc<Self>> {
        Self::open(root, existing, true, None)?
            .ok_or_else(|| Error::new("package_store_unavailable"))
    }
    pub(super) fn open_for_import_accounted(
        root: DirectoryLease,
        existing: Arc<LiveRoots>,
        control: Arc<crate::runtime_packages::memory::Controller>,
    ) -> Result<Arc<Self>> {
        Self::open(root, existing, true, Some(control))?
            .ok_or_else(|| Error::new("package_store_unavailable"))
    }
    fn open(
        root: DirectoryLease,
        existing: Arc<LiveRoots>,
        create: bool,
        control: Option<Arc<crate::runtime_packages::memory::Controller>>,
    ) -> Result<Option<Arc<Self>>> {
        let mut bootstrap = if create {
            control
                .as_ref()
                .map(|memory| memory.bootstrap())
                .transpose()
                .map_err(|error| Error::new(error.code))?
        } else {
            None
        };
        let owner = match root.acquire_private_child(&existing, Path::new(STORE), false) {
            Ok(owner) => owner,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                existing
                    .verify()
                    .map_err(|_| Error::new("storage_root_unavailable"))?;
                match std::fs::symlink_metadata(root.root().join(STORE)) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        existing
                            .verify()
                            .map_err(|_| Error::new("storage_root_unavailable"))?;
                        if !create {
                            return Ok(None);
                        }
                        root.acquire_private_child(&existing, Path::new(STORE), true)
                            .map_err(storage)?
                    }
                    // A dedicated engines volume arrives as an empty directory with no control file.
                    Ok(metadata)
                        if metadata.is_dir() && empty_directory(&root.root().join(STORE)) =>
                    {
                        if !create {
                            return Ok(None);
                        }
                        root.acquire_private_child(&existing, Path::new(STORE), true)
                            .map_err(storage)?
                    }
                    _ => return Err(Error::new("package_store_unavailable")),
                }
            }
            Err(error) => return Err(storage(error)),
        };
        if bootstrap.is_none() {
            bootstrap = control
                .as_ref()
                .map(|memory| memory.bootstrap())
                .transpose()
                .map_err(|error| Error::new(error.code))?;
        }
        let scope = existing.extend(owner.clone()).map_err(storage)?;
        let observed =
            match owner.read_private_metadata(scope.clone(), Path::new(MANIFEST), MAX_MANIFEST) {
                Ok(file) => Some(file),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(storage(error)),
            };
        let manifest = match &observed {
            Some(file) => serde_json::from_slice::<Manifest>(file.bytes())
                .map_err(|_| Error::new("manifest_invalid"))?,
            None => Manifest {
                schema_version: 1,
                generation: 0,
                packages: Vec::new(),
            },
        };
        manifest.validate()?;
        validate_index(&owner, &scope, &manifest)?;
        let observed = observed
            .map(crate::storage::MetadataFile::into_observation)
            .transpose()
            .map_err(storage)?;
        let store = Arc::new(Self {
            owner,
            scope,
            current: std::sync::RwLock::new(Arc::new(Head {
                observed,
                manifest,
                uncertain: None,
            })),
            publishing: std::sync::atomic::AtomicBool::new(false),
            failed: std::sync::atomic::AtomicBool::new(false),
            importing: std::sync::Mutex::new(()),
            _memory: bootstrap
                .map(|claim| claim.metadata())
                .transpose()
                .map_err(|error| Error::new(error.code))?,
        });
        store.verify()?;
        Ok(Some(store))
    }
    fn current(&self) -> Result<Arc<Head>> {
        self.current
            .read()
            .map(|head| head.clone())
            .map_err(|_| Error::new("package_owner_unavailable"))
    }
    fn roots(&self) -> Result<()> {
        self.scope
            .verify()
            .map_err(|_| Error::new("storage_root_unavailable"))
    }
    fn fault(&self, head: &Head) -> Result<()> {
        if let Some((error, _, _)) = &head.uncertain {
            return Err(*error);
        }
        if self.failed.load(Ordering::Acquire) {
            return Err(Error::new("manifest_durability_uncertain"));
        }
        Ok(())
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.roots()?;
        let head = self.current()?;
        self.fault(&head)?;
        // Owned index publications preserve every retained package record and payload.
        if self.publishing.load(Ordering::Acquire) {
            return self.roots();
        }
        let checked = self.verify_head(&head);
        if self.publishing.load(Ordering::Acquire) {
            return self.roots();
        }
        let current = self.current()?;
        self.fault(&current)?;
        if !Arc::ptr_eq(&head, &current) {
            return self.roots();
        }
        checked
    }
    pub(crate) fn publication_uncertain(&self) -> bool {
        self.failed.load(Ordering::Acquire)
    }
    fn head(&self) -> Result<Arc<Head>> {
        self.roots()?;
        for _ in 0..3 {
            let head = self.current()?;
            self.fault(&head)?;
            if self.publishing.load(Ordering::Acquire) {
                return Err(Error::new("runtime_index_publishing"));
            }
            let checked = self.verify_head(&head);
            if self.publishing.load(Ordering::Acquire) {
                return Err(Error::new("runtime_index_publishing"));
            }
            if !Arc::ptr_eq(&head, &self.current()?) {
                continue;
            }
            self.fault(&head)?;
            checked?;
            return Ok(head);
        }
        Err(Error::new("runtime_index_publishing"))
    }
    fn verify_head(&self, head: &Head) -> Result<()> {
        match &head.observed {
            Some(file) => file.verify().map_err(storage),
            None => match self.owner.observe_private_metadata(
                self.scope.clone(),
                Path::new(MANIFEST),
                MAX_MANIFEST,
            ) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                _ => Err(Error::new("manifest_changed")),
            },
        }
    }
    fn replace(&self, next: Arc<Head>) -> Result<()> {
        let old = {
            let mut current = self
                .current
                .write()
                .map_err(|_| Error::new("package_owner_unavailable"))?;
            std::mem::replace(&mut *current, next)
        };
        drop(old);
        Ok(())
    }
    pub(super) fn enter_import(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.importing.try_lock().map_err(|error| {
            Error::new(match error {
                std::sync::TryLockError::WouldBlock => "runtime_import_busy",
                std::sync::TryLockError::Poisoned(_) => "package_owner_unavailable",
            })
        })
    }
    pub(super) fn snapshot(&self) -> Result<Manifest> {
        let head = self.head()?;
        validate_index(&self.owner, &self.scope, &head.manifest)?;
        Ok(head.manifest.clone())
    }
    pub(super) fn publish(
        &self,
        next: Manifest,
        operation: &Operation,
        checkpoint: impl FnOnce(),
    ) -> Result<()> {
        operation.check().map_err(storage)?;
        next.validate()?;
        crate::storage::check_json_size(&next, MAX_MANIFEST)
            .map_err(|_| Error::new("manifest_limit"))?;
        let mut digest = Sha256::new();
        serde_json::to_writer(HashWriter(&mut digest), &next)
            .map_err(|_| Error::new("manifest_invalid"))?;
        digest.update(b"\n");
        let digest = format!("{:x}", digest.finalize());
        let head = self.head()?;
        if head.manifest.generation.checked_add(1) != Some(next.generation) {
            return Err(Error::new("manifest_revision_conflict"));
        }
        for current in &head.manifest.packages {
            let Some(retained) = next
                .packages
                .iter()
                .find(|package| package.id == current.id)
            else {
                return Err(Error::new("package_record_changed"));
            };
            if !Arc::ptr_eq(&retained.specification, &current.specification)
                || retained.reserved_bytes != current.reserved_bytes
                || (current.phase == Phase::Installed && retained.phase != Phase::Installed)
            {
                return Err(Error::new("package_record_changed"));
            }
        }
        {
            let current = self
                .current
                .write()
                .map_err(|_| Error::new("package_owner_unavailable"))?;
            if !Arc::ptr_eq(&current, &head) || self.publishing.swap(true, Ordering::AcqRel) {
                return Err(Error::new("runtime_index_publishing"));
            }
        }
        let mut publication = Publication {
            store: self,
            known: false,
        };
        checkpoint();
        if let Err(error) = self
            .verify_head(&head)
            .and_then(|()| super::commit(&self.owner, &self.scope, &next, operation))
        {
            if matches!(error.reason(), "cancelled" | "deadline") {
                publication.known = true;
            } else {
                self.replace(Arc::new(Head {
                    observed: None,
                    manifest: head.manifest.clone(),
                    uncertain: Some((error, next, head.clone())),
                }))?;
            }
            return Err(error);
        }
        let observed = self
            .owner
            .observe_private_metadata(self.scope.clone(), Path::new(MANIFEST), MAX_MANIFEST)
            .and_then(|mut observed| {
                observed.verify_digest(&digest)?;
                Ok(observed)
            });
        match observed {
            Ok(observed) => {
                self.replace(Arc::new(Head {
                    observed: Some(observed),
                    manifest: next,
                    uncertain: None,
                }))?;
                publication.known = true;
                Ok(())
            }
            Err(error) => {
                let error = storage(error);
                self.replace(Arc::new(Head {
                    observed: None,
                    manifest: head.manifest.clone(),
                    uncertain: Some((error, next, head.clone())),
                }))?;
                Err(error)
            }
        }
    }
    /// Removes an unleased package. The payload goes before its index record: an interrupted
    /// removal leaves an indexed package whose payload no longer verifies, which a retry
    /// finishes, never an unindexed directory that would stop the store from opening.
    pub(crate) fn remove(&self, id: &str, expected_generation: u64) -> Result<u64> {
        if !valid_id(id) {
            return Err(Error::new("invalid_package_id"));
        }
        let _import = self.enter_import()?;
        let head = self.head()?;
        if head.manifest.generation != expected_generation {
            return Err(Error::new("manifest_revision_conflict"));
        }
        let package = head
            .manifest
            .packages
            .iter()
            .find(|package| package.id == id)
            .ok_or_else(|| Error::new("package_not_found"))?;
        if package.specification.binding_leases.load(Ordering::Acquire) != 0 {
            return Err(Error::new("package_in_use"));
        }
        let reserved = package.reserved_bytes;
        self.roots()?;
        let payload = self.owner.root().join(id);
        match std::fs::symlink_metadata(&payload) {
            Ok(metadata) if metadata.is_dir() => {
                writable_tree(&payload).map_err(storage)?;
                std::fs::remove_dir_all(&payload).map_err(storage)?;
            }
            Ok(_) => return Err(Error::new("store_unindexed")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(storage(error)),
        }
        let mut next = head.manifest.clone();
        next.packages.retain(|package| package.id != id);
        next.generation = next
            .generation
            .checked_add(1)
            .ok_or_else(|| Error::new("generation_exhausted"))?;
        next.validate()?;
        {
            let current = self
                .current
                .write()
                .map_err(|_| Error::new("package_owner_unavailable"))?;
            if !Arc::ptr_eq(&current, &head) || self.publishing.swap(true, Ordering::AcqRel) {
                return Err(Error::new("runtime_index_publishing"));
            }
        }
        let mut publication = Publication {
            store: self,
            known: false,
        };
        let operation = super::Operation {
            deadline: Instant::now() + std::time::Duration::from_secs(60),
            cancelled: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        };
        super::commit(&self.owner, &self.scope, &next, &operation)?;
        let observed = self
            .owner
            .observe_private_metadata(self.scope.clone(), Path::new(MANIFEST), MAX_MANIFEST)
            .map_err(storage)?;
        self.replace(Arc::new(Head {
            observed: Some(observed),
            manifest: next,
            uncertain: None,
        }))?;
        publication.known = true;
        Ok(reserved)
    }
    pub(crate) fn inventory(&self) -> Result<serde_json::Value> {
        let head = self.head()?;
        let packages: Vec<_> = head.manifest.packages.iter().map(|package| serde_json::json!({
            "id": package.id, "phase": package.phase, "reserved_bytes": package.reserved_bytes,
            "implementation_id": package.specification.bundle.implementation_id,
            "provenance": package.specification.provenance, "declared_target": package.specification.declared_target,
            "declared_host_match": package.specification.declared_target.matches_service_target(),
            "member_count": package.specification.files.len(), "compatibility":"unqualified",
            "binding_leases": package.specification.binding_leases.load(Ordering::Acquire),
            "verification":"required-on-acquire"
        })).collect();
        Ok(
            serde_json::json!({"schema_version":1,"generation":head.manifest.generation,"packages":packages,"read_only":true,"compatibility":"unqualified"}),
        )
    }
    pub(crate) fn hold(
        self: &Arc<Self>,
        id: &str,
        provider: crate::config::Provider,
        deadline: Instant,
    ) -> Result<Arc<Lease>> {
        if !valid_id(id) {
            return Err(Error::new("invalid_package_id"));
        }
        let binding = {
            let head = self.head()?;
            let current = self
                .current
                .read()
                .map_err(|_| Error::new("package_owner_unavailable"))?;
            self.fault(&current)?;
            if self.publishing.load(Ordering::Acquire) || !Arc::ptr_eq(&head, &current) {
                return Err(Error::new("runtime_index_publishing"));
            }
            let package = head
                .manifest
                .packages
                .iter()
                .find(|package| package.id == id)
                .ok_or_else(|| Error::new("package_not_found"))?;
            if package.phase != Phase::Installed {
                return Err(Error::new("package_incomplete"));
            }
            if provider.implementation_id()
                != Some(package.specification.bundle.implementation_id.as_str())
            {
                return Err(Error::new("package_adapter_mismatch"));
            }
            if !package
                .specification
                .declared_target
                .matches_service_target()
            {
                return Err(Error::new("package_target_mismatch"));
            }
            if Instant::now() >= deadline {
                return Err(Error::new("deadline"));
            }
            BindingLease::acquire(package.specification.clone())?
        };
        let specification = &binding.0.request;
        let encoded = crate::storage::json_size(specification, MAX_MANIFEST)
            .map_err(|_| Error::new("manifest_limit"))?;
        let root_bytes = (self.owner.root().as_os_str().len() as u64)
            .checked_add(id.len() as u64 + 10)
            .ok_or_else(|| Error::new("runtime_memory_limits"))?;
        let retained = encoded
            .checked_mul(4)
            .and_then(|bytes| {
                root_bytes
                    .checked_mul(
                        (specification.bundle.files.len()
                            + specification.bundle.library_directories.len()
                            + 8) as u64,
                    )?
                    .checked_mul(4)?
                    .checked_add(bytes)
            })
            .and_then(|bytes| bytes.checked_add(1024 * 1024))
            .ok_or_else(|| Error::new("runtime_memory_limits"))?;
        let memory = self
            ._memory
            .as_ref()
            .map(|claim| claim.controller().binding(retained))
            .transpose()
            .map_err(|error| Error::new(error.code))?;
        let owner = self
            .owner
            .acquire_private_child(&self.scope, Path::new(id), false)
            .map_err(storage)?;
        let scope = self.scope.extend(owner.clone()).map_err(storage)?;
        let payload = RuntimePayload::open(owner, scope, false).map_err(storage)?;
        let mut bundle = specification.bundle.clone();
        bundle.root = payload.path();
        bundle
            .verify(deadline)
            .map_err(|_| Error::new("payload_verification"))?;
        verify_sealed(&payload, specification)?;
        self.verify()?;
        payload.verify().map_err(storage)?;
        Ok(Arc::new(Lease {
            store: self.clone(),
            id: id.to_owned(),
            payload,
            bundle,
            _binding: binding,
            _memory: memory
                .map(|claim| claim.binding(retained))
                .transpose()
                .map_err(|error| Error::new(error.code))?,
        }))
    }
}
impl Lease {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }
    pub(crate) fn bundle(&self) -> &BundleSpec {
        &self.bundle
    }
    pub(crate) fn verify(&self) -> Result<()> {
        self.store.verify()?;
        self.payload.verify().map_err(storage)
    }
}

fn empty_directory(path: &Path) -> bool {
    std::fs::read_dir(path).is_ok_and(|mut entries| entries.next().is_none())
}

/// Published payloads are read-only; directories must be writable before their entries unlink.
fn writable_tree(root: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut pending = vec![root.to_owned()];
    while let Some(directory) = pending.pop() {
        let metadata = std::fs::symlink_metadata(&directory)?;
        if !metadata.is_dir() {
            continue;
        }
        let mode = metadata.permissions().mode();
        if mode & 0o700 != 0o700 {
            std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(mode | 0o700))?;
        }
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}
