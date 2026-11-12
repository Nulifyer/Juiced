use super::inventory::{
    ERROR_PREVIEW_BYTES, MAXIMUM_PAGE_SIZE, REFERENCE_PREVIEW, RESPONSE_BYTES,
    SOURCE_URL_PREVIEW_BYTES,
};
use super::sets::{ArtifactRole, ArtifactSetMember, ArtifactSetOrigin, ArtifactSetSpec};
use super::*;

fn digest(index: usize) -> String {
    format!("{:064x}", index + 1)
}
fn job_key(index: usize) -> String {
    format!("{:032}", index)
}
fn set_key(index: usize) -> String {
    format!("set-{}", digest(index))
}
fn artifact(bytes: u64, references: impl IntoIterator<Item = String>) -> Artifact {
    Artifact {
        bytes,
        sha256: String::new(),
        references: references.into_iter().collect(),
        removing: false,
        holds: None,
    }
}
fn job(source_url: Option<String>, error: Option<String>) -> Job {
    Job {
        revision: 1,
        state: JobState::Failed,
        source_url,
        expected_sha256: digest(0),
        expected_bytes: Some(1),
        maximum_bytes: 1,
        downloaded_bytes: 0,
        validator: None,
        artifact: None,
        error,
        restart: false,
    }
}
fn specification(members: impl IntoIterator<Item = String>, label: &str) -> ArtifactSetSpec {
    let members: Vec<_> = members
        .into_iter()
        .enumerate()
        .map(|(index, artifact)| ArtifactSetMember {
            name: format!("{label}{index:0>width$}", width = 64 - label.len()),
            role: if index == 0 {
                ArtifactRole::Model
            } else {
                ArtifactRole::VoiceConfiguration
            },
            artifact,
            bytes: u64::MAX,
        })
        .collect();
    ArtifactSetSpec {
        schema_version: 1,
        origin: ArtifactSetOrigin {
            importer_id: "i".repeat(128),
            importer_version: "v".repeat(256),
            source_revision: "r".repeat(256),
        },
        primary: members[0].name.clone(),
        members,
    }
}
fn install(store: &ArtifactStore, edit: impl FnOnce(&mut Manifest)) {
    let mut runtime = store.runtime.lock().unwrap();
    let mut next = runtime.manifest.clone();
    edit(&mut next);
    next.generation += 1;
    for (id, artifact) in &mut next.artifacts {
        artifact.sha256.clone_from(id);
    }
    runtime.replace_manifest(next);
}
fn request(section: Option<InventorySection>, limit: usize) -> InventoryRequest {
    InventoryRequest {
        section,
        limit: Some(limit),
        ..InventoryRequest::default()
    }
}

#[test]
fn sections_page_in_id_order_and_continuations_require_the_same_membership_view() {
    let (_directory, store) = tests::store();
    install(&store, |manifest| {
        for index in 0..7 {
            manifest.artifacts.insert(digest(index), artifact(1, []));
            manifest.jobs.insert(job_key(index), job(None, None));
        }
    });

    let mut seen = Vec::new();
    let mut page = store
        .inventory(&request(Some(InventorySection::Artifacts), 3))
        .unwrap();
    assert!(page.jobs.is_empty() && page.pages.jobs.is_none());
    let view = page.view.clone();
    loop {
        let info = page.pages.artifacts.as_ref().unwrap();
        assert_eq!(info.total, 7);
        assert!(page.artifacts.len() <= 3);
        seen.extend(page.artifacts.iter().map(|row| row.artifact.id.clone()));
        let Some(after) = info.next_after.clone() else {
            break;
        };
        install(&store, |manifest| {
            manifest.jobs.get_mut(&job_key(0)).unwrap().downloaded_bytes += 1;
        });
        page = store
            .inventory(&InventoryRequest {
                artifacts_after: Some(after),
                expected_view: Some(view.clone()),
                ..request(Some(InventorySection::Artifacts), 3)
            })
            .unwrap();
        assert_eq!(page.view, view, "progress must not invalidate a walk");
    }
    assert_eq!(seen, (0..7).map(digest).collect::<Vec<_>>());

    let first = store
        .inventory(&request(Some(InventorySection::Jobs), 2))
        .unwrap();
    let after = first.pages.jobs.unwrap().next_after.unwrap();
    assert_eq!(after, job_key(1));
    install(&store, |manifest| {
        manifest.artifacts.insert(digest(99), artifact(1, []));
    });
    let stale = InventoryRequest {
        jobs_after: Some(after.clone()),
        expected_view: Some(first.view.clone()),
        ..request(Some(InventorySection::Jobs), 2)
    };
    assert!(matches!(
        store.inventory(&stale),
        Err(ArtifactError::Conflict)
    ));

    let current = store.inventory(&request(None, 1)).unwrap();
    assert_ne!(current.view, first.view);
    let missing = InventoryRequest {
        jobs_after: Some(job_key(50)),
        expected_view: Some(current.view.clone()),
        ..request(Some(InventorySection::Jobs), 2)
    };
    assert!(matches!(
        store.inventory(&missing),
        Err(ArtifactError::NotFound)
    ));
    let resumed = store
        .inventory(&InventoryRequest {
            jobs_after: Some(after),
            expected_view: Some(current.view.clone()),
            ..request(Some(InventorySection::Jobs), 2)
        })
        .unwrap();
    assert_eq!(
        resumed
            .jobs
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>(),
        [job_key(2), job_key(3)]
    );

    let independent = store
        .inventory(&InventoryRequest {
            jobs_after: Some(job_key(4)),
            artifacts_after: Some(digest(1)),
            expected_view: Some(current.view),
            ..request(None, 2)
        })
        .unwrap();
    assert_eq!(
        independent
            .jobs
            .iter()
            .map(|job| job.id.as_str())
            .collect::<Vec<_>>(),
        [job_key(5), job_key(6)]
    );
    assert_eq!(
        independent
            .artifacts
            .iter()
            .map(|row| row.artifact.id.as_str())
            .collect::<Vec<_>>(),
        [digest(2), digest(3)]
    );
    assert_eq!(independent.pages.jobs.unwrap().next_after, None);
    assert_eq!(
        independent.pages.artifacts.unwrap().next_after,
        Some(digest(3))
    );
    assert!(independent.pages.artifact_sets.is_some());
}

#[test]
fn malformed_requests_are_rejected_before_reading_state() {
    let (_directory, store) = tests::store();
    let view = store.inventory(&InventoryRequest::default()).unwrap().view;
    let invalid = [
        request(None, 0),
        request(None, MAXIMUM_PAGE_SIZE + 1),
        InventoryRequest {
            artifacts_after: Some(digest(0)),
            expected_view: Some(view.clone()),
            ..request(Some(InventorySection::Jobs), 1)
        },
        InventoryRequest {
            artifacts_after: Some(digest(0)),
            ..request(Some(InventorySection::Artifacts), 1)
        },
        InventoryRequest {
            artifacts_after: Some("not-a-digest".into()),
            expected_view: Some(view.clone()),
            ..request(Some(InventorySection::Artifacts), 1)
        },
        InventoryRequest {
            jobs_after: Some(digest(0)),
            expected_view: Some(view.clone()),
            ..request(None, 1)
        },
        InventoryRequest {
            artifact_sets_after: Some(digest(0)),
            expected_view: Some(view.clone()),
            ..request(None, 1)
        },
        InventoryRequest {
            quarantined_files_after: Some(String::new()),
            expected_view: Some(view.clone()),
            ..request(None, 1)
        },
        InventoryRequest {
            expected_view: Some("short".into()),
            ..InventoryRequest::default()
        },
        InventoryRequest {
            artifact_set: Some(digest(0)),
            ..InventoryRequest::default()
        },
        InventoryRequest {
            artifact: Some(digest(0xabc).to_ascii_uppercase()),
            ..InventoryRequest::default()
        },
    ];
    for request in invalid {
        assert!(
            matches!(store.inventory(&request), Err(ArtifactError::Invalid)),
            "{request:?}"
        );
    }
}

#[test]
fn references_count_each_set_once_and_selection_survives_off_page_rows() {
    let (_directory, store) = tests::store();
    let shared = digest(500);
    install(&store, |manifest| {
        for index in 0..4 {
            manifest.artifacts.insert(digest(index), artifact(1, []));
        }
        manifest.artifacts.insert(
            shared.clone(),
            artifact(1, (0..10).map(|index| format!("model:direct-{index:02}"))),
        );
        for index in 0..12 {
            let members = [shared.clone(), shared.clone(), digest(index % 4)];
            let references = (0..20).map(|reference| format!("model:{index}-{reference:02}"));
            manifest.artifact_sets.insert(
                set_key(index),
                sets::record(specification(members, "m"), references.collect()),
            );
        }
    });

    let page = store
        .inventory(&InventoryRequest {
            artifact: Some(shared.clone()),
            artifact_set: Some(set_key(11)),
            ..request(Some(InventorySection::Artifacts), 2)
        })
        .unwrap();
    assert_eq!(page.artifacts.len(), 2);
    assert!(page.artifacts.iter().all(|row| row.artifact.id != shared));
    for row in &page.artifacts {
        assert_eq!(row.reference_count, 3, "{}", row.artifact.id);
    }
    let selected = page.selection.artifact.unwrap();
    assert_eq!(selected.reference_count, 10 + 12);
    assert_eq!(selected.artifact.references.len(), REFERENCE_PREVIEW);
    assert!(selected.artifact.references[0].starts_with("model:direct-"));
    let set = page.selection.artifact_set.unwrap();
    assert_eq!(set.id, set_key(11));
    assert_eq!(
        (set.references.len(), set.reference_count),
        (REFERENCE_PREVIEW, 20)
    );
    assert!(
        !set.members_present,
        "member byte counts differ from stored files"
    );

    let whole = store
        .inventory(&request(
            Some(InventorySection::Artifacts),
            MAXIMUM_PAGE_SIZE,
        ))
        .unwrap();
    let listed = whole
        .artifacts
        .iter()
        .find(|row| row.artifact.id == shared)
        .unwrap();
    assert_eq!(listed.reference_count, selected.reference_count);
    assert_eq!(listed.artifact.references, selected.artifact.references);

    let absent = store
        .inventory(&InventoryRequest {
            artifact: Some(digest(9_000)),
            artifact_set: Some(set_key(9_000)),
            ..request(None, 1)
        })
        .unwrap();
    assert!(absent.selection.artifact.is_none() && absent.selection.artifact_set.is_none());
}

#[test]
fn a_maximal_page_with_selections_fits_the_response_bound() {
    let (_directory, store) = tests::store();
    let escaped_reference = "\"".repeat(256);
    let long_url = format!(
        "https://example.test/{}é",
        "\\".repeat(SOURCE_URL_PREVIEW_BYTES)
    );
    install(&store, |manifest| {
        let limits = ArtifactLimits::default();
        for index in 0..limits.maximum_artifacts {
            let references = if index < MAXIMUM_PAGE_SIZE + 1 {
                (0..64)
                    .map(|reference| format!("{reference:03}{escaped_reference}"))
                    .collect()
            } else {
                Vec::new()
            };
            manifest
                .artifacts
                .insert(digest(index), artifact(u64::MAX, references));
        }
        for index in 0..limits.maximum_jobs {
            manifest.jobs.insert(
                job_key(index),
                job(Some(long_url.clone()), Some("\"".repeat(4096))),
            );
        }
        for index in 0..limits.maximum_sets {
            let members = (0..64).map(|member| digest((index * 64 + member) % 256));
            let references = (0..64).map(|reference| format!("{reference:03}{escaped_reference}"));
            manifest.artifact_sets.insert(
                set_key(index),
                sets::record(specification(members, "member-"), references.collect()),
            );
        }
    });

    let page = store
        .inventory(&InventoryRequest {
            limit: Some(MAXIMUM_PAGE_SIZE),
            artifact: Some(digest(MAXIMUM_PAGE_SIZE)),
            artifact_set: Some(set_key(1_000)),
            ..InventoryRequest::default()
        })
        .unwrap();
    assert_eq!(page.jobs.len(), MAXIMUM_PAGE_SIZE);
    assert_eq!(page.artifacts.len(), MAXIMUM_PAGE_SIZE);
    assert_eq!(page.artifact_sets.len(), MAXIMUM_PAGE_SIZE);
    let job = &page.jobs[0];
    assert!(job.source_url_truncated);
    assert_eq!(job.source_url_bytes, long_url.len());
    assert!(job.source_url.as_ref().unwrap().len() <= SOURCE_URL_PREVIEW_BYTES);
    assert!(job.error.as_ref().unwrap().len() <= ERROR_PREVIEW_BYTES);
    assert!(
        page.artifacts
            .iter()
            .all(|row| row.artifact.references.len() == REFERENCE_PREVIEW)
    );
    assert!(
        page.artifact_sets
            .iter()
            .all(|set| set.references.len() == REFERENCE_PREVIEW)
    );

    let bytes = crate::storage::json_size(&page, RESPONSE_BYTES)
        .unwrap_or_else(|_| panic!("maximal inventory page exceeds {RESPONSE_BYTES} bytes"));
    assert!(
        bytes > RESPONSE_BYTES / 4,
        "fixture no longer approaches the bound: {bytes}"
    );
    eprintln!("maximal artifact inventory page: {bytes} of {RESPONSE_BYTES} bytes");
}

#[test]
fn previews_truncate_on_character_boundaries() {
    let value = format!("{}é", "a".repeat(SOURCE_URL_PREVIEW_BYTES - 1));
    let preview = inventory::truncate(&value, SOURCE_URL_PREVIEW_BYTES);
    assert_eq!(preview.len(), SOURCE_URL_PREVIEW_BYTES - 1);
    assert_eq!(inventory::truncate("short", 8), "short");
}
