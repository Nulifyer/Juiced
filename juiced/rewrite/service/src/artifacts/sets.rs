use super::*;
use crate::identity::VerifiedFile;

const MAX_MEMBERS: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactBinding {
    pub artifact: String,
    pub artifact_set: String,
}
impl ArtifactBinding {
    pub(crate) fn validate(&self) -> Result<(), ArtifactError> {
        artifact_id(&self.artifact)?;
        set_id(&self.artifact_set)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactRole {
    Model,
    WeightShard,
    WeightIndex,
    Tokenizer,
    Configuration,
    Processor,
    Projector,
    DraftModel,
    Adapter,
    Voice,
    VoiceConfiguration,
    Vocabulary,
    ExternalTensor,
    Encoder,
    Decoder,
    Denoiser,
    Vae,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSetOrigin {
    pub importer_id: String,
    pub importer_version: String,
    pub source_revision: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSetMember {
    pub name: String,
    pub role: ArtifactRole,
    pub artifact: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactSetSpec {
    pub schema_version: u32,
    pub origin: ArtifactSetOrigin,
    pub primary: String,
    pub members: Vec<ArtifactSetMember>,
}

impl ArtifactSetSpec {
    fn canonical(mut self, limits: &ArtifactLimits) -> Result<Self, ArtifactError> {
        if self.schema_version != 1
            || self.members.is_empty()
            || self.members.len() > MAX_MEMBERS
            || !label(&self.primary, 64)
            || !label(&self.origin.importer_id, 128)
            || !revision(&self.origin.importer_version)
            || !revision(&self.origin.source_revision)
        {
            return Err(ArtifactError::Invalid);
        }
        self.members
            .sort_by(|left, right| left.name.cmp(&right.name));
        let mut previous = None;
        let mut unique = BTreeMap::new();
        let mut primary = false;
        for member in &self.members {
            artifact_id(&member.artifact)?;
            if !label(&member.name, 64)
                || previous == Some(member.name.as_str())
                || member.bytes == 0
                || member.bytes > limits.maximum_artifact_bytes
                || ((member.role == ArtifactRole::Model) != (member.name == self.primary))
            {
                return Err(ArtifactError::Invalid);
            }
            if let Some(bytes) = unique.insert(member.artifact.as_str(), member.bytes)
                && bytes != member.bytes
            {
                return Err(ArtifactError::Invalid);
            }
            primary |= member.name == self.primary;
            previous = Some(member.name.as_str());
        }
        let total = unique
            .values()
            .try_fold(0u64, |total, bytes| total.checked_add(*bytes));
        if !primary || total.is_none_or(|total| total > limits.maximum_total_bytes) {
            return Err(ArtifactError::Invalid);
        }
        Ok(self)
    }

    fn identity(&self) -> Result<String, ArtifactError> {
        let bytes = serde_json::to_vec(&("juiced-artifact-set-v1", self))
            .map_err(|_| ArtifactError::Invalid)?;
        Ok(format!("set-{:x}", Sha256::digest(bytes)))
    }
}

fn label(value: &str, maximum: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
}
fn revision(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._:+-@".contains(&byte))
}
fn set_id(value: &str) -> Result<(), ArtifactError> {
    artifact_id(value.strip_prefix("set-").ok_or(ArtifactError::Invalid)?)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SetRecord {
    pub(super) specification: Arc<ArtifactSetSpec>,
    references: BTreeSet<String>,
    #[serde(skip)]
    holds: Option<Arc<AtomicUsize>>,
}

#[derive(Clone, Debug, Serialize)]
pub struct ArtifactSetView {
    pub id: String,
    pub specification: Arc<ArtifactSetSpec>,
    pub references: Vec<String>,
    pub reference_count: usize,
    pub active_holds: usize,
    pub members_present: bool,
    pub compatibility: &'static str,
    pub content_verification: &'static str,
}
pub(super) fn bounded_view(id: &str, record: &SetRecord, manifest: &Manifest) -> ArtifactSetView {
    let members_present = record.specification.members.iter().all(|member| {
        manifest
            .artifacts
            .get(&member.artifact)
            .is_some_and(|artifact| artifact.bytes == member.bytes && !artifact.removing)
    });
    ArtifactSetView {
        id: id.to_owned(),
        specification: record.specification.clone(),
        references: record
            .references
            .iter()
            .take(super::inventory::REFERENCE_PREVIEW)
            .cloned()
            .collect(),
        reference_count: record.references.len(),
        active_holds: hold_count(&record.holds),
        members_present,
        compatibility: "unqualified",
        content_verification: "required-on-acquire",
    }
}
#[cfg(test)]
pub(super) fn record(specification: ArtifactSetSpec, references: BTreeSet<String>) -> SetRecord {
    SetRecord {
        specification: Arc::new(specification),
        references,
        holds: None,
    }
}
pub(super) fn validate_id(value: &str) -> Result<(), ArtifactError> {
    set_id(value)
}

#[derive(Clone)]
pub struct SetOperation {
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}
impl SetOperation {
    pub fn new(deadline: Instant, cancelled: Arc<AtomicBool>) -> Self {
        Self {
            deadline,
            cancelled,
        }
    }
    pub fn check(&self) -> Result<(), ArtifactError> {
        if self.cancelled.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        if Instant::now() >= self.deadline {
            return Err(ArtifactError::Deadline);
        }
        Ok(())
    }
    fn io_check(&self) -> io::Result<()> {
        self.check().map_err(|error| {
            io::Error::new(
                if matches!(error, ArtifactError::Deadline) {
                    io::ErrorKind::TimedOut
                } else {
                    io::ErrorKind::Interrupted
                },
                "Artifact set verification interrupted",
            )
        })
    }
}

pub struct ArtifactSetLease {
    _hold: Hold,
    id: String,
    specification: Arc<ArtifactSetSpec>,
    members: Vec<ArtifactLease>,
    verified: Vec<VerifiedFile>,
}
impl ArtifactSetLease {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn specification(&self) -> &ArtifactSetSpec {
        &self.specification
    }
    pub fn member(&self, name: &str) -> Option<&ArtifactLease> {
        let member = self
            .specification
            .members
            .iter()
            .find(|member| member.name == name)?;
        self.members
            .iter()
            .find(|held| held.artifact.id == member.artifact)
    }
    pub fn primary(&self) -> &ArtifactLease {
        self.member(&self.specification.primary)
            .expect("validated set primary")
    }
    pub fn verify(&self) -> Result<(), ArtifactError> {
        for (held, verified) in self.members.iter().zip(&self.verified) {
            held._store.verify_mutation()?;
            verified
                .verify_current()
                .map_err(|_| ArtifactError::Corrupt)?;
        }
        Ok(())
    }
}

impl ArtifactStore {
    fn verify_set_members(
        self: &Arc<Self>,
        spec: &ArtifactSetSpec,
        operation: &SetOperation,
    ) -> Result<(Vec<ArtifactLease>, Vec<VerifiedFile>), ArtifactError> {
        operation.check()?;
        let unique: BTreeSet<_> = spec
            .members
            .iter()
            .map(|member| member.artifact.as_str())
            .collect();
        let mut held = Vec::with_capacity(unique.len());
        let mut verified = Vec::with_capacity(unique.len());
        for id in unique {
            operation.check()?;
            if self.stopping.load(Ordering::Acquire) {
                return Err(ArtifactError::Stop);
            }
            self.verify_mutation()?;
            let lease = self.reserve_lease(id)?;
            let expected = spec
                .members
                .iter()
                .find(|member| member.artifact == id)
                .expect("set member");
            if lease.artifact.bytes != expected.bytes {
                return Err(ArtifactError::SizeMismatch);
            }
            let source = regular(lease.path())?;
            if source.metadata()?.len() != expected.bytes {
                return Err(ArtifactError::SizeMismatch);
            }
            let file =
                VerifiedFile::from_file_checked(lease.path(), source, operation.deadline, || {
                    operation.io_check()?;
                    if self.stopping.load(Ordering::Acquire) {
                        return Err(io::Error::from(io::ErrorKind::Interrupted));
                    }
                    Ok(())
                })
                .map_err(|error| match error.kind() {
                    io::ErrorKind::TimedOut => ArtifactError::Deadline,
                    io::ErrorKind::Interrupted => ArtifactError::Stop,
                    _ if crate::storage::is_root_unavailable(&error) => {
                        ArtifactError::RootUnavailable
                    }
                    _ => ArtifactError::Corrupt,
                })?;
            self.verify_mutation()?;
            if file.digest() != id || file.bytes() != expected.bytes {
                return Err(ArtifactError::HashMismatch);
            }
            held.push(lease);
            verified.push(file);
        }
        operation.check()?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        Ok((held, verified))
    }

    pub fn set_operation(&self, cancelled: Arc<AtomicBool>) -> SetOperation {
        SetOperation::new(
            Instant::now() + self.limits.request_timeout.min(Duration::from_secs(30)),
            cancelled,
        )
    }

    pub fn publish_set(
        self: &Arc<Self>,
        specification: ArtifactSetSpec,
        expected_generation: u64,
        operation: &SetOperation,
    ) -> Result<ArtifactSetView, ArtifactError> {
        self.publish_set_checked(specification, expected_generation, operation, || {})
    }

    pub(super) fn publish_set_checked(
        self: &Arc<Self>,
        specification: ArtifactSetSpec,
        expected_generation: u64,
        operation: &SetOperation,
        mut before_publication: impl FnMut(),
    ) -> Result<ArtifactSetView, ArtifactError> {
        operation.check()?;
        let specification = Arc::new(specification.canonical(&self.limits)?);
        let _work = self.set_work()?;
        let id = specification.identity()?;
        operation.check()?;
        self.verify_mutation()?;
        {
            let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
            if runtime.manifest.generation != expected_generation {
                return Err(ArtifactError::Conflict);
            }
            if !runtime.manifest.artifact_sets.contains_key(&id)
                && runtime.manifest.artifact_sets.len() >= self.limits.maximum_sets
            {
                return Err(ArtifactError::Limit);
            }
        }
        let (held, verified) = self.verify_set_members(&specification, operation)?;
        before_publication();
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        operation.check()?;
        self.verify_mutation()?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        if runtime.manifest.generation != expected_generation {
            return Err(ArtifactError::Conflict);
        }
        for file in &verified {
            file.verify_current().map_err(|_| ArtifactError::Corrupt)?;
        }
        if let Some(record) = runtime.manifest.artifact_sets.get(&id) {
            return Ok(bounded_view(&id, record, &runtime.manifest));
        }
        if runtime.manifest.artifact_sets.len() >= self.limits.maximum_sets {
            return Err(ArtifactError::Limit);
        }
        let mut next = runtime.manifest.clone();
        next.artifact_sets.insert(
            id.clone(),
            SetRecord {
                specification,
                references: BTreeSet::new(),
                holds: None,
            },
        );
        self.commit(&mut runtime, next)?;
        let result = bounded_view(&id, &runtime.manifest.artifact_sets[&id], &runtime.manifest);
        drop(runtime);
        drop(held);
        Ok(result)
    }

    pub fn artifact_set(&self, id: &str) -> Result<ArtifactSetView, ArtifactError> {
        set_id(id)?;
        self.verify_mutation()?;
        let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        Ok(bounded_view(
            id,
            runtime
                .manifest
                .artifact_sets
                .get(id)
                .ok_or(ArtifactError::NotFound)?,
            &runtime.manifest,
        ))
    }

    pub fn hold_set(
        self: &Arc<Self>,
        id: &str,
        operation: &SetOperation,
    ) -> Result<ArtifactSetLease, ArtifactError> {
        set_id(id)?;
        operation.check()?;
        let _work = self.set_work()?;
        self.verify_mutation()?;
        let (specification, hold) = {
            let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
            let record = runtime
                .manifest
                .artifact_sets
                .get_mut(id)
                .ok_or(ArtifactError::NotFound)?;
            (
                record.specification.clone(),
                Hold::acquire(&mut record.holds)?,
            )
        };
        let (members, verified) = self.verify_set_members(&specification, operation)?;
        let lease = ArtifactSetLease {
            _hold: hold,
            id: id.to_owned(),
            specification,
            members,
            verified,
        };
        lease.verify()?;
        Ok(lease)
    }

    pub fn hold_binding(
        self: &Arc<Self>,
        artifact: &str,
        set: Option<&str>,
    ) -> Result<ArtifactLease, ArtifactError> {
        artifact_id(artifact)?;
        let Some(id) = set else {
            return self.hold(artifact);
        };
        let view = self.artifact_set(id)?;
        if view.specification.members.len() != 1 {
            return Err(ArtifactError::SetUnsupported);
        }
        if view.specification.members[0].artifact != artifact {
            return Err(ArtifactError::SetBinding);
        }
        let held =
            Arc::new(self.hold_set(id, &self.set_operation(Arc::new(AtomicBool::new(false))))?);
        let primary = held.primary();
        Ok(ArtifactLease {
            _store: self.clone(),
            _hold: None,
            path: primary.path.clone(),
            artifact: primary.artifact.clone(),
            set: Some(held),
        })
    }

    pub fn reference_binding(
        &self,
        artifact: &str,
        set: Option<&str>,
        reference: &str,
        add: bool,
        expected_generation: u64,
    ) -> Result<ArtifactView, ArtifactError> {
        artifact_id(artifact)?;
        if !reference_valid(reference) {
            return Err(ArtifactError::Invalid);
        }
        if let Some(id) = set {
            set_id(id)?;
        }
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        if runtime.manifest.generation != expected_generation {
            return Err(ArtifactError::Conflict);
        }
        let current = runtime
            .manifest
            .artifacts
            .get(artifact)
            .filter(|record| !record.removing)
            .ok_or(ArtifactError::NotFound)?;
        if let Some(id) = set {
            let record = runtime
                .manifest
                .artifact_sets
                .get(id)
                .ok_or(ArtifactError::NotFound)?;
            let primary = record
                .specification
                .members
                .iter()
                .find(|member| member.name == record.specification.primary)
                .expect("validated primary");
            if primary.artifact != artifact {
                return Err(ArtifactError::SetBinding);
            }
            if current.references.contains(reference) == add
                && record.references.contains(reference) == add
            {
                return Ok(artifact_view(artifact, current));
            }
        }
        let mut next = runtime.manifest.clone();
        let primary = next.artifacts.get_mut(artifact).expect("existing artifact");
        if add {
            primary.references.insert(reference.to_owned());
        } else {
            primary.references.remove(reference);
        }
        if let Some(id) = set {
            let record = next.artifact_sets.get_mut(id).expect("existing set");
            if add {
                record.references.insert(reference.to_owned());
            } else {
                record.references.remove(reference);
            }
        }
        self.commit(&mut runtime, next)?;
        Ok(artifact_view(
            artifact,
            &runtime.manifest.artifacts[artifact],
        ))
    }

    pub fn remove_set(&self, id: &str, expected_generation: u64) -> Result<(), ArtifactError> {
        set_id(id)?;
        self.verify_mutation()?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        if runtime.manifest.generation != expected_generation {
            return Err(ArtifactError::Conflict);
        }
        let record = runtime
            .manifest
            .artifact_sets
            .get(id)
            .ok_or(ArtifactError::NotFound)?;
        if !record.references.is_empty() || hold_count(&record.holds) != 0 {
            return Err(ArtifactError::Busy);
        }
        let mut next = runtime.manifest.clone();
        next.artifact_sets.remove(id);
        self.commit(&mut runtime, next)
    }

    pub fn reference_bindings(
        &self,
        reference: &str,
        bindings: &[ArtifactBinding],
        expected_generation: u64,
    ) -> Result<(), ArtifactError> {
        if !reference_valid(reference) || bindings.len() > 3 {
            return Err(ArtifactError::Invalid);
        }
        self.verify_mutation()?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        if self.stopping.load(Ordering::Acquire) {
            return Err(ArtifactError::Stop);
        }
        if runtime.manifest.generation != expected_generation {
            return Err(ArtifactError::Conflict);
        }
        let mut artifacts = BTreeSet::new();
        let mut sets = BTreeSet::new();
        for binding in bindings {
            binding.validate()?;
            let record = runtime
                .manifest
                .artifact_sets
                .get(&binding.artifact_set)
                .ok_or(ArtifactError::NotFound)?;
            let primary = record
                .specification
                .members
                .iter()
                .find(|member| member.name == record.specification.primary)
                .expect("validated primary");
            if primary.artifact != binding.artifact {
                return Err(ArtifactError::SetBinding);
            }
            artifacts.insert(binding.artifact.as_str());
            sets.insert(binding.artifact_set.as_str());
        }
        let changed = runtime.manifest.artifacts.iter().any(|(id, record)| {
            record.references.contains(reference) != artifacts.contains(id.as_str())
        }) || runtime.manifest.artifact_sets.iter().any(|(id, record)| {
            record.references.contains(reference) != sets.contains(id.as_str())
        });
        if !changed {
            return Ok(());
        }
        let mut next = runtime.manifest.clone();
        for (id, record) in &mut next.artifacts {
            if artifacts.contains(id.as_str()) {
                record.references.insert(reference.to_owned());
            } else {
                record.references.remove(reference);
            }
        }
        for (id, record) in &mut next.artifact_sets {
            if sets.contains(id.as_str()) {
                record.references.insert(reference.to_owned());
            } else {
                record.references.remove(reference);
            }
        }
        self.commit(&mut runtime, next)
    }
}

pub(super) fn validate_records(
    manifest: &Manifest,
    limits: &ArtifactLimits,
) -> Result<(), ArtifactError> {
    if manifest.artifact_sets.len() > limits.maximum_sets {
        return Err(ArtifactError::Limit);
    }
    for (id, record) in &manifest.artifact_sets {
        set_id(id)?;
        let canonical = (*record.specification).clone().canonical(limits)?;
        if canonical != *record.specification
            || canonical.identity()? != *id
            || record
                .references
                .iter()
                .any(|reference| !reference_valid(reference))
        {
            return Err(ArtifactError::Corrupt);
        }
        for member in &canonical.members {
            let artifact = manifest
                .artifacts
                .get(&member.artifact)
                .ok_or(ArtifactError::Corrupt)?;
            if artifact.removing || artifact.bytes != member.bytes {
                return Err(ArtifactError::Corrupt);
            }
        }
    }
    Ok(())
}

pub(super) fn contains(manifest: &Manifest, id: &str) -> bool {
    manifest.artifact_sets.values().any(|record| {
        record
            .specification
            .members
            .iter()
            .any(|member| member.artifact == id)
    })
}
#[cfg(test)]
pub(super) fn views(manifest: &Manifest) -> Vec<ArtifactSetView> {
    manifest
        .artifact_sets
        .iter()
        .map(|(id, record)| bounded_view(id, record, manifest))
        .collect()
}
#[cfg(test)]
pub(super) fn membership(manifest: &Manifest) -> BTreeMap<&str, Vec<&str>> {
    let mut index: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (id, record) in &manifest.artifact_sets {
        for member in &record.specification.members {
            let sets = index.entry(&member.artifact).or_default();
            if sets.last().copied() != Some(id.as_str()) {
                sets.push(id);
            }
        }
    }
    index
}
