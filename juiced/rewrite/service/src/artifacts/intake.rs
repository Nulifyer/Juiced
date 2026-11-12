use super::*;

/// Placeholder for jobs whose digest is learned from the bytes they receive; it is replaced
/// before publication and never names a stored artifact.
const UNLEARNED_DIGEST: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const CHECKPOINT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct Published {
    pub job: String,
    pub artifact: String,
    pub bytes: u64,
    pub duplicate: bool,
}

/// A local upload streamed straight into a job's staging file. Dropping it before
/// [`Intake::finish`] removes the staging file and the job record.
pub struct Intake {
    store: Arc<ArtifactStore>,
    id: String,
    control: Arc<Control>,
    file: Option<File>,
    digest: Sha256,
    bytes: u64,
    declared: u64,
    checkpoint: (u64, Instant),
    settled: bool,
}

impl ArtifactStore {
    pub fn begin_intake(
        self: &Arc<Self>,
        source_name: String,
        declared: u64,
    ) -> Result<Intake, ArtifactError> {
        validate_expected(UNLEARNED_DIGEST, Some(declared), declared, &self.limits)?;
        let (view, control) = self.begin_with(
            None,
            UNLEARNED_DIGEST.to_owned(),
            Some(declared),
            declared,
            None,
            Some(source_name),
        )?;
        let mut intake = Intake {
            store: self.clone(),
            id: view.id,
            control,
            file: None,
            digest: Sha256::new(),
            bytes: 0,
            declared,
            checkpoint: (0, Instant::now()),
            settled: false,
        };
        self.set_state(&intake.id, JobState::Downloading, None)?;
        self.verify_mutation()?;
        let path = self.staging_path(&intake.id);
        let file = fs::OpenOptions::new().write(true).open(&path)?;
        if !same_file(&file, &path)? || file.metadata()?.len() != 0 {
            return Err(ArtifactError::Corrupt);
        }
        intake.file = Some(file);
        Ok(intake)
    }

    /// Copies an already-open local file as an ordinary resumable import job whose digest is
    /// computed while copying.
    pub fn start_copy(
        self: &Arc<Self>,
        file: File,
        source_name: String,
    ) -> Result<JobView, ArtifactError> {
        let metadata = file.metadata()?;
        let bytes = metadata.len();
        if !metadata.is_file() {
            return Err(ArtifactError::Invalid);
        }
        validate_expected(UNLEARNED_DIGEST, Some(bytes), bytes, &self.limits)?;
        let (view, control) = self.begin_with(
            None,
            UNLEARNED_DIGEST.to_owned(),
            Some(bytes),
            bytes,
            None,
            Some(source_name),
        )?;
        let store = self.clone();
        let id = view.id.clone();
        tokio::spawn(async move {
            store.run(id, control, Some(file)).await;
        });
        Ok(view)
    }

    pub fn available_bytes(&self) -> Option<u64> {
        available_bytes(self.lease.root())
    }

    pub fn has_artifact(&self, digest: &str) -> bool {
        self.runtime.lock().is_ok_and(|runtime| {
            runtime
                .manifest
                .artifacts
                .get(digest)
                .is_some_and(|artifact| !artifact.removing)
        })
    }

    pub(super) fn learn_digest(&self, id: &str, digest: &str) -> Result<(), ArtifactError> {
        artifact_id(digest)?;
        self.verify_mutation()?;
        let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
        let job = runtime
            .manifest
            .jobs
            .get(id)
            .ok_or(ArtifactError::NotFound)?;
        if !job.computed_digest {
            return Err(ArtifactError::HashMismatch);
        }
        if job.expected_sha256 == digest {
            return Ok(());
        }
        let mut next = runtime.manifest.clone();
        let job = next.jobs.get_mut(id).expect("computed job");
        job.expected_sha256 = digest.to_owned();
        job.revision = job.revision.checked_add(1).ok_or(ArtifactError::Limit)?;
        self.commit(&mut runtime, next)
    }

    fn discard_intake(&self, id: &str, control: &Control) {
        let removed = (|| {
            self.verify_mutation()?;
            let path = self.staging_path(id);
            match regular(&path) {
                Ok(_) => fs::remove_file(path)?,
                Err(ArtifactError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let mut runtime = self.runtime.lock().map_err(|_| ArtifactError::Corrupt)?;
            if runtime
                .manifest
                .jobs
                .get(id)
                .is_some_and(|job| job.state != JobState::Complete)
            {
                let mut next = runtime.manifest.clone();
                next.jobs.remove(id);
                self.commit(&mut runtime, next)?;
            }
            Ok::<_, ArtifactError>(())
        })();
        self.release_intake(id, control, removed.err().map(|error| error.code()));
    }

    fn release_intake(&self, id: &str, control: &Control, failure: Option<&'static str>) {
        if let Ok(mut runtime) = self.runtime.lock() {
            runtime.active.remove(id);
            match failure {
                Some(code) => {
                    runtime.failures.insert(id.to_owned(), code);
                }
                None => {
                    runtime.failures.remove(id);
                }
            }
        }
        control.done.store(true, Ordering::Release);
        control.finished.notify_waiters();
    }
}

impl Intake {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn received(&self) -> u64 {
        self.bytes
    }

    pub fn staging_path(&self) -> PathBuf {
        self.store.staging_path(&self.id)
    }

    pub fn running(&self) -> bool {
        self.control.command.load(Ordering::Acquire) == RUN
    }

    /// Resolves when an operator cancel, pause or service shutdown asks the upload to stop.
    pub async fn stopped(&self) {
        loop {
            let changed = self.control.changed.notified();
            if !self.running() {
                return;
            }
            changed.await;
        }
    }

    pub fn write(&mut self, chunk: &[u8]) -> Result<(), ArtifactError> {
        if !self.running() {
            return Err(ArtifactError::Stop);
        }
        let next = self
            .bytes
            .checked_add(chunk.len() as u64)
            .filter(|bytes| *bytes <= self.declared)
            .ok_or(ArtifactError::SizeMismatch)?;
        self.store.verify_mutation()?;
        let file = self.file.as_mut().ok_or(ArtifactError::Corrupt)?;
        file.write_all(chunk)?;
        self.digest.update(chunk);
        self.bytes = next;
        if self.bytes - self.checkpoint.0 >= CHECKPOINT_BYTES
            || self.checkpoint.1.elapsed() >= Duration::from_secs(1)
        {
            self.store.checkpoint(&self.id, self.bytes, None)?;
            self.checkpoint = (self.bytes, Instant::now());
        }
        Ok(())
    }

    pub fn finish(mut self) -> Result<Published, ArtifactError> {
        if self.bytes != self.declared {
            return Err(ArtifactError::SizeMismatch);
        }
        if !self.running() {
            return Err(ArtifactError::Stop);
        }
        let file = self.file.take().ok_or(ArtifactError::Corrupt)?;
        self.store.verify_mutation()?;
        file.sync_all()?;
        self.store.checkpoint(&self.id, self.bytes, None)?;
        let actual = format!("{:x}", std::mem::take(&mut self.digest).finalize());
        self.store.learn_digest(&self.id, &actual)?;
        let duplicate = self.store.has_artifact(&actual);
        self.store
            .publish(&self.id, self.bytes, &actual, &file, &self.control)?;
        self.settled = true;
        self.store.release_intake(&self.id, &self.control, None);
        Ok(Published {
            job: self.id.clone(),
            artifact: actual,
            bytes: self.bytes,
            duplicate,
        })
    }
}

impl Drop for Intake {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.file.take();
        let store = self.store.clone();
        let id = std::mem::take(&mut self.id);
        let control = self.control.clone();
        let discard = move || store.discard_intake(&id, &control);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) => {
                handle.spawn_blocking(discard);
            }
            Err(_) => discard(),
        }
    }
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)]
fn available_bytes(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut status = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `path` is NUL-terminated and `status` is writable storage for one statvfs.
    if unsafe { libc::statvfs(path.as_ptr(), status.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs returned success, so it initialized `status`.
    let status = unsafe { status.assume_init() };
    Some((status.f_bavail as u64).saturating_mul(status.f_frsize as u64))
}

#[cfg(not(unix))]
fn available_bytes(_: &Path) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(directory: &Path) -> Arc<ArtifactStore> {
        let lease = DirectoryLease::acquire(directory.join("artifacts")).unwrap();
        ArtifactStore::open(lease, ArtifactLimits::default(), SourcePolicy::Offline).unwrap()
    }

    fn staged(directory: &Path) -> Vec<String> {
        fs::read_dir(directory.join("artifacts"))
            .unwrap()
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".part"))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn uploads_learn_their_digest_dedupe_and_keep_their_file_name() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let bytes = b"GGUF uploaded bytes".to_vec();
        let expected = format!("{:x}", Sha256::digest(&bytes));
        let mut intake = store
            .begin_intake("tiny.gguf".into(), bytes.len() as u64)
            .unwrap();
        intake.write(&bytes[..4]).unwrap();
        intake.write(&bytes[4..]).unwrap();
        let first = intake.finish().unwrap();
        assert_eq!(first.artifact, expected);
        assert!(!first.duplicate);
        assert_eq!(
            store.source_file_name(&expected).as_deref(),
            Some("tiny.gguf")
        );
        let job = store.job(&first.job).unwrap();
        assert_eq!(job.state, JobState::Complete);
        assert_eq!(job.expected_sha256, expected);
        assert!(job.computed_digest);
        let mut again = store
            .begin_intake("copy.gguf".into(), bytes.len() as u64)
            .unwrap();
        again.write(&bytes).unwrap();
        let second = again.finish().unwrap();
        assert_eq!(second.artifact, expected);
        assert!(second.duplicate);
        assert!(staged(directory.path()).is_empty());
        assert_eq!(store.inspect().unwrap().artifacts.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn abandoned_or_oversized_uploads_leave_no_staging_or_job() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let mut intake = store.begin_intake("partial.gguf".into(), 8).unwrap();
        let id = intake.id().to_owned();
        intake.write(b"GGUF").unwrap();
        assert!(matches!(
            intake.write(b"too many bytes"),
            Err(ArtifactError::SizeMismatch)
        ));
        drop(intake);
        for _ in 0..200 {
            if store.job(&id).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(matches!(store.job(&id), Err(ArtifactError::NotFound)));
        assert!(staged(directory.path()).is_empty());
        let short = store.begin_intake("short.gguf".into(), 8).unwrap();
        assert!(matches!(short.finish(), Err(ArtifactError::SizeMismatch)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(store.inspect().unwrap().jobs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn copy_jobs_compute_digest_and_report_progress() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(directory.path());
        let source = directory.path().join("source.gguf");
        let bytes = vec![7u8; 3 * 1024 * 1024];
        fs::write(&source, &bytes).unwrap();
        let view = store
            .start_copy(File::open(&source).unwrap(), "source.gguf".into())
            .unwrap();
        assert!(view.computed_digest);
        assert_eq!(view.expected_bytes, Some(bytes.len() as u64));
        let settled = store.wait(&view.id).await.unwrap();
        assert_eq!(settled.state, JobState::Complete, "{settled:?}");
        assert_eq!(settled.downloaded_bytes, bytes.len() as u64);
        let digest = format!("{:x}", Sha256::digest(&bytes));
        assert_eq!(settled.artifact.as_deref(), Some(digest.as_str()));
        assert!(source.is_file());
        assert!(store.available_bytes().is_some_and(|bytes| bytes > 0));
    }
}
