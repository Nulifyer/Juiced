use super::*;
use std::sync::mpsc;

fn stage(store: &ArtifactStore, bytes: &[u8]) -> (JobView, Arc<Control>, File) {
    let digest = format!("{:x}", Sha256::digest(bytes));
    let length = bytes.len() as u64;
    let (job, control) = store
        .begin(None, digest, Some(length), length, store.generation())
        .unwrap();
    let path = store.staging_path(&job.id);
    fs::write(&path, bytes).unwrap();
    store
        .set_state(&job.id, JobState::Downloading, None)
        .unwrap();
    (
        store.job(&job.id).unwrap(),
        control,
        File::open(path).unwrap(),
    )
}

fn installed(store: &ArtifactStore) -> String {
    let (job, control, file) = stage(store, b"safe");
    store
        .publish(&job.id, 4, &job.expected_sha256, &file, &control)
        .unwrap();
    store.runtime.lock().unwrap().active.remove(&job.id);
    job.expected_sha256
}

#[test]
fn duplicate_verification_keeps_inventory_controls_and_references_available() {
    for command in [RUN, PAUSE, CANCEL] {
        let (_directory, store) = tests::store();
        let digest = installed(&store);
        let (job, control, file) = stage(&store, b"safe");
        let (entered, observed) = mpsc::sync_channel(0);
        let (release, resumed) = mpsc::sync_channel(0);
        let publishing = store.clone();
        let id = job.id.clone();
        let retained = control.clone();
        let worker = std::thread::spawn(move || {
            publishing.publish_checked(&id, 4, &digest, &file, &retained, || {
                entered.send(()).unwrap();
                resumed.recv_timeout(Duration::from_secs(5)).unwrap();
            })
        });
        observed.recv_timeout(Duration::from_secs(5)).unwrap();
        let unlocked = store.runtime.try_lock().is_ok();
        if !unlocked {
            release.send(()).unwrap();
            let _ = worker.join();
            panic!("artifact verification retained the metadata mutex");
        }
        let view = store.inspect().unwrap();
        let artifact = &view.artifacts[0];
        assert_eq!(artifact.active_holds, 1);
        assert_eq!(store.job(&job.id).unwrap().state, JobState::Downloading);
        assert!(matches!(
            store.remove(&artifact.id, view.generation),
            Err(ArtifactError::Busy)
        ));
        let referenced = store
            .reference(&artifact.id, "profile:concurrent", true, view.generation)
            .unwrap();
        assert_eq!(referenced.references, ["profile:concurrent"]);
        let request = match command {
            PAUSE => Some(store.pause(&job.id, job.revision).unwrap()),
            CANCEL => Some(store.cancel(&job.id, job.revision).unwrap()),
            _ => None,
        };
        if let Some(request) = request {
            assert!(request.writer_active);
            assert!(matches!(
                store.forget_job(&job.id, request.revision),
                Err(ArtifactError::Busy)
            ));
        }
        release.send(()).unwrap();
        let result = worker.join().unwrap();
        if command == RUN {
            result.unwrap();
        } else {
            assert!(matches!(result, Err(ArtifactError::Stop)));
        }
        store
            .settle_transfer(
                &job.id,
                command,
                if command == RUN {
                    Ok(())
                } else {
                    Err(ArtifactError::Stop)
                },
            )
            .unwrap();
        store.runtime.lock().unwrap().active.remove(&job.id);
        let settled = store.job(&job.id).unwrap();
        assert_eq!(
            settled.state,
            match command {
                PAUSE => JobState::Paused,
                CANCEL => JobState::Cancelled,
                _ => JobState::Complete,
            }
        );
        assert!(!settled.writer_active);
        let view = store.inspect().unwrap();
        assert_eq!(view.artifacts.len(), 1);
        assert_eq!(view.artifacts[0].references, ["profile:concurrent"]);
        assert_eq!(view.artifacts[0].active_holds, 0);
        assert_eq!(
            fs::read(store.artifact_path(&view.artifacts[0].id)).unwrap(),
            b"safe"
        );
        assert_eq!(store.staging_path(&job.id).exists(), command == PAUSE);
    }
}

#[test]
fn checked_hash_stops_between_reads_without_publishing_or_deleting_files() {
    let (directory, store) = tests::store();
    let path = directory.path().join("hash-source");
    let bytes = vec![3u8; 8192 * 3];
    fs::write(&path, &bytes).unwrap();
    let mut checks = 0;
    let result = hash_checked(&path, bytes.len() as u64, || {
        checks += 1;
        if checks == 3 {
            Err(ArtifactError::Stop)
        } else {
            Ok(())
        }
    });
    assert!(matches!(result, Err(ArtifactError::Stop)));
    assert_eq!(checks, 3);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(store.inspect().unwrap().jobs.is_empty());
}

#[test]
fn duplicate_verification_failure_releases_hold_and_preserves_both_files() {
    let (_directory, store) = tests::store();
    let digest = installed(&store);
    fs::write(store.artifact_path(&digest), b"evil").unwrap();
    let (job, control, file) = stage(&store, b"safe");
    assert!(matches!(
        store.publish(&job.id, 4, &digest, &file, &control),
        Err(ArtifactError::Corrupt)
    ));
    let view = store.inspect().unwrap();
    assert_eq!(view.artifacts[0].active_holds, 0);
    assert_eq!(store.job(&job.id).unwrap().state, JobState::Downloading);
    assert_eq!(fs::read(store.artifact_path(&digest)).unwrap(), b"evil");
    assert_eq!(fs::read(store.staging_path(&job.id)).unwrap(), b"safe");
}

#[test]
fn concurrent_same_digest_publications_keep_one_immutable_artifact() {
    let (_directory, store) = tests::store();
    let first = stage(&store, b"safe");
    let second = stage(&store, b"safe");
    let gate = Arc::new(std::sync::Barrier::new(3));
    let workers: Vec<_> = [first, second]
        .into_iter()
        .map(|(job, control, file)| {
            let owner = store.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                owner
                    .publish(&job.id, 4, &job.expected_sha256, &file, &control)
                    .unwrap();
                owner.runtime.lock().unwrap().active.remove(&job.id);
                assert!(!owner.staging_path(&job.id).exists());
            })
        })
        .collect();
    gate.wait();
    for worker in workers {
        worker.join().unwrap();
    }
    let inventory = store.inspect().unwrap();
    assert_eq!(inventory.jobs.len(), 2);
    assert!(
        inventory
            .jobs
            .iter()
            .all(|job| job.state == JobState::Complete && !job.writer_active)
    );
    assert_eq!(inventory.artifacts.len(), 1);
    assert_eq!(inventory.artifacts[0].active_holds, 0);
    assert_eq!(inventory.charged_bytes, 4);
    assert_eq!(
        fs::read(store.artifact_path(&inventory.artifacts[0].id)).unwrap(),
        b"safe"
    );
}

#[cfg(unix)]
#[test]
fn duplicate_verification_refuses_replaced_root_without_touching_either_tree() {
    let (directory, store) = tests::store();
    let digest = installed(&store);
    let (job, control, file) = stage(&store, b"safe");
    let manifest = fs::read(store.lease.root().join(MANIFEST)).unwrap();
    let retained = directory.path().join("retained-artifacts");
    let result = store.publish_checked(&job.id, 4, &digest, &file, &control, || {
        fs::rename(store.lease.root(), &retained).unwrap();
        fs::create_dir(store.lease.root()).unwrap();
        fs::write(store.artifact_path(&digest), b"foreign").unwrap();
    });
    assert!(matches!(result, Err(ArtifactError::RootUnavailable)));
    assert_eq!(fs::read(store.artifact_path(&digest)).unwrap(), b"foreign");
    assert_eq!(
        fs::read(retained.join(format!("artifact-{digest}.bin"))).unwrap(),
        b"safe"
    );
    assert_eq!(
        fs::read(retained.join(format!("job-{}.part", job.id))).unwrap(),
        b"safe"
    );
    assert_eq!(fs::read(retained.join(MANIFEST)).unwrap(), manifest);
    assert_eq!(
        store.runtime.lock().unwrap().manifest.artifacts[&digest].active_holds(),
        0
    );
}
