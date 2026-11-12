use super::*;
use std::ops::Bound;

pub const DEFAULT_PAGE_SIZE: usize = 64;
pub const MAXIMUM_PAGE_SIZE: usize = 128;
pub const REFERENCE_PREVIEW: usize = 8;
pub const SOURCE_URL_PREVIEW_BYTES: usize = 2048;
pub const ERROR_PREVIEW_BYTES: usize = 256;
pub const RESPONSE_BYTES: u64 = 6 * 1024 * 1024;
pub const RESPONSE_WORK_BYTES: u64 = 2 * RESPONSE_BYTES;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Section {
    Jobs,
    Artifacts,
    ArtifactSets,
    QuarantinedFiles,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct InventoryRequest {
    pub limit: Option<usize>,
    pub section: Option<Section>,
    pub jobs_after: Option<String>,
    pub artifacts_after: Option<String>,
    pub artifact_sets_after: Option<String>,
    pub quarantined_files_after: Option<String>,
    pub expected_view: Option<String>,
    pub artifact: Option<String>,
    pub artifact_set: Option<String>,
}

impl InventoryRequest {
    fn limit(&self) -> usize {
        self.limit.unwrap_or(DEFAULT_PAGE_SIZE)
    }

    fn validate(&self) -> Result<(), ArtifactError> {
        if self.limit() == 0 || self.limit() > MAXIMUM_PAGE_SIZE {
            return Err(ArtifactError::Invalid);
        }
        if self
            .expected_view
            .as_deref()
            .is_some_and(|view| !digest_valid(view))
        {
            return Err(ArtifactError::Invalid);
        }
        for section in [
            Section::Jobs,
            Section::Artifacts,
            Section::ArtifactSets,
            Section::QuarantinedFiles,
        ] {
            let Some(after) = self.after(section) else {
                continue;
            };
            if self.expected_view.is_none() || !self.includes(section) {
                return Err(ArtifactError::Invalid);
            }
            match section {
                Section::Jobs => job_id(after)?,
                Section::Artifacts => artifact_id(after)?,
                Section::ArtifactSets => sets::validate_id(after)?,
                Section::QuarantinedFiles if after.is_empty() || after.len() > 255 => {
                    return Err(ArtifactError::Invalid);
                }
                Section::QuarantinedFiles => {}
            }
        }
        if let Some(artifact) = &self.artifact {
            artifact_id(artifact)?;
        }
        if let Some(set) = &self.artifact_set {
            sets::validate_id(set)?;
        }
        Ok(())
    }

    fn after(&self, section: Section) -> Option<&str> {
        match section {
            Section::Jobs => &self.jobs_after,
            Section::Artifacts => &self.artifacts_after,
            Section::ArtifactSets => &self.artifact_sets_after,
            Section::QuarantinedFiles => &self.quarantined_files_after,
        }
        .as_deref()
    }

    fn includes(&self, section: Section) -> bool {
        self.section.is_none_or(|selected| selected == section)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct InventoryArtifact {
    #[serde(flatten)]
    pub artifact: ArtifactView,
    pub reference_count: usize,
}

#[derive(Debug, Serialize)]
pub struct PageInfo {
    pub total: usize,
    pub next_after: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Pages {
    pub jobs: Option<PageInfo>,
    pub artifacts: Option<PageInfo>,
    pub artifact_sets: Option<PageInfo>,
    pub quarantined_files: Option<PageInfo>,
}

#[derive(Debug, Default, Serialize)]
pub struct Selection {
    pub artifact: Option<InventoryArtifact>,
    pub artifact_set: Option<ArtifactSetView>,
}

#[derive(Debug, Serialize)]
pub struct InventoryPage {
    pub schema_version: u32,
    pub generation: u64,
    pub view: String,
    pub limit: usize,
    pub source_policy: SourcePolicy,
    pub read_only: bool,
    pub recovery_authorized: bool,
    pub charged_bytes: u64,
    pub jobs: Vec<JobView>,
    pub artifacts: Vec<InventoryArtifact>,
    pub artifact_sets: Vec<ArtifactSetView>,
    pub quarantined_files: Vec<QuarantinedFileView>,
    pub pages: Pages,
    pub selection: Selection,
}

pub(super) fn truncate(value: &str, maximum: usize) -> &str {
    if value.len() <= maximum {
        return value;
    }
    let mut end = maximum;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

pub(super) fn membership_changed(previous: &Manifest, next: &Manifest) -> bool {
    !previous.jobs.keys().eq(next.jobs.keys())
        || !previous.artifacts.keys().eq(next.artifacts.keys())
        || !previous.artifact_sets.keys().eq(next.artifact_sets.keys())
}

fn view_token(runtime: &Runtime) -> String {
    let mut hash = Sha256::new();
    hash.update(b"juiced-artifact-page-v1\0");
    hash.update(runtime.instance.as_bytes());
    hash.update(runtime.membership_epoch.to_le_bytes());
    for count in [
        runtime.manifest.jobs.len(),
        runtime.manifest.artifacts.len(),
        runtime.manifest.artifact_sets.len(),
        runtime.quarantined_files.len(),
    ] {
        hash.update((count as u64).to_le_bytes());
    }
    format!("{:x}", hash.finalize())
}

fn artifact_row(id: &str, artifact: &Artifact) -> InventoryArtifact {
    let mut references = Vec::with_capacity(REFERENCE_PREVIEW.min(artifact.references.len()));
    references.extend(artifact.references.iter().take(REFERENCE_PREVIEW).cloned());
    InventoryArtifact {
        artifact: ArtifactView {
            id: id.to_owned(),
            bytes: artifact.bytes,
            sha256: artifact.sha256.clone(),
            references,
            active_holds: artifact.active_holds(),
            removing: artifact.removing,
        },
        reference_count: artifact.references.len(),
    }
}

// Rows arrive in manifest id order, which the binary search depends on.
fn attach_set_references(rows: &mut [&mut InventoryArtifact], manifest: &Manifest) {
    if rows.is_empty() {
        return;
    }
    let mut counted = vec![usize::MAX; rows.len()];
    for (position, (set, record)) in manifest.artifact_sets.iter().enumerate() {
        for member in &record.specification.members {
            let Ok(index) =
                rows.binary_search_by(|row| row.artifact.id.as_str().cmp(&member.artifact))
            else {
                continue;
            };
            if counted[index] == position {
                continue;
            }
            counted[index] = position;
            let row = &mut rows[index];
            row.reference_count += 1;
            if row.artifact.references.len() < REFERENCE_PREVIEW {
                row.artifact.references.push(format!("artifact-set:{set}"));
            }
        }
    }
}

fn page<'a, V: 'a, R>(
    map: &'a BTreeMap<String, V>,
    after: Option<&str>,
    limit: usize,
    mut row: impl FnMut(&'a str, &'a V) -> R,
) -> Result<(Vec<R>, PageInfo), ArtifactError> {
    if let Some(after) = after
        && !map.contains_key(after)
    {
        return Err(ArtifactError::NotFound);
    }
    let start = after.map_or(Bound::Unbounded, Bound::Excluded);
    let mut entries = map.range::<str, _>((start, Bound::Unbounded));
    let mut rows = Vec::with_capacity(limit.min(map.len()));
    let mut last = None;
    for (id, value) in entries.by_ref().take(limit) {
        last = Some(id);
        rows.push(row(id, value));
    }
    let next_after = entries.next().and_then(|_| last.cloned());
    Ok((
        rows,
        PageInfo {
            total: map.len(),
            next_after,
        },
    ))
}

impl ArtifactStore {
    pub fn inventory(&self, request: &InventoryRequest) -> Result<InventoryPage, ArtifactError> {
        request.validate()?;
        self.lease.verify()?;
        let runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let view = view_token(&runtime);
        if request
            .expected_view
            .as_ref()
            .is_some_and(|expected| *expected != view)
        {
            return Err(ArtifactError::Conflict);
        }
        let limit = request.limit();
        let after = |section| request.after(section);
        let manifest = &runtime.manifest;
        let mut pages = Pages::default();
        let mut inventory = InventoryPage {
            schema_version: 2,
            generation: manifest.generation,
            view,
            limit,
            source_policy: self.policy,
            read_only: true,
            recovery_authorized: false,
            charged_bytes: charged(&runtime),
            jobs: Vec::new(),
            artifacts: Vec::new(),
            artifact_sets: Vec::new(),
            quarantined_files: Vec::new(),
            pages: Pages::default(),
            selection: Selection::default(),
        };
        if request.includes(Section::Jobs) {
            let (rows, info) = page(&manifest.jobs, after(Section::Jobs), limit, |id, job| {
                job_view(id, job, &runtime)
            })?;
            inventory.jobs = rows;
            pages.jobs = Some(info);
        }
        if request.includes(Section::Artifacts) {
            let (rows, info) = page(
                &manifest.artifacts,
                after(Section::Artifacts),
                limit,
                artifact_row,
            )?;
            inventory.artifacts = rows;
            pages.artifacts = Some(info);
        }
        if request.includes(Section::ArtifactSets) {
            let (rows, info) = page(
                &manifest.artifact_sets,
                after(Section::ArtifactSets),
                limit,
                |id, record| sets::bounded_view(id, record, manifest),
            )?;
            inventory.artifact_sets = rows;
            pages.artifact_sets = Some(info);
        }
        if request.includes(Section::QuarantinedFiles) {
            let (rows, info) = page(
                &runtime.quarantined_files,
                after(Section::QuarantinedFiles),
                limit,
                |filename, reserved_bytes| QuarantinedFileView {
                    filename: filename.to_owned(),
                    reserved_bytes: *reserved_bytes,
                    reason: "unreferenced_generated_file",
                },
            )?;
            inventory.quarantined_files = rows;
            pages.quarantined_files = Some(info);
        }
        inventory.pages = pages;
        let mut selected = request
            .artifact
            .as_deref()
            .and_then(|id| manifest.artifacts.get_key_value(id))
            .map(|(id, artifact)| artifact_row(id, artifact));
        let mut rows: Vec<&mut InventoryArtifact> = inventory.artifacts.iter_mut().collect();
        attach_set_references(&mut rows, manifest);
        if let Some(row) = selected.as_mut() {
            attach_set_references(&mut [row], manifest);
        }
        inventory.selection = Selection {
            artifact: selected,
            artifact_set: request.artifact_set.as_deref().and_then(|id| {
                manifest
                    .artifact_sets
                    .get(id)
                    .map(|record| sets::bounded_view(id, record, manifest))
            }),
        };
        Ok(inventory)
    }
}
