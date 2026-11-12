use crate::{
    native_settings::{Document, MAX_DOCUMENT_BYTES, Role},
    storage::{DirectoryReader, ReadObservation},
};
pub mod descriptor;
pub mod generations;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const MAX_COHORT_BYTES: usize = 64 * 1024 * 1024;
const SOURCE_ROLES: [Role; 6] = [
    Role::Config,
    Role::Jobs,
    Role::RecipeOptions,
    Role::UserModels,
    Role::ExecutionProfiles,
    Role::NativeHelpers,
];
const UNSUPPORTED_SOURCE_ENTRIES: [&str; 3] = [
    "juiced",
    ".juiced-configuration-migration.json",
    ".juiced-configuration-generation",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Reason {
    SourceUnavailable,
    UnsupportedSourceGeneration,
    CohortTooLarge,
    Cancelled,
    Deadline,
    Document(crate::native_settings::Reason),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Error {
    pub role: Option<Role>,
    pub reason: Reason,
}

impl Error {
    pub fn code(&self) -> &'static str {
        "configuration_migration_recovery_required"
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(
            "Native metadata inspection failed; preserve the source and target directories.",
        )
    }
}

impl std::error::Error for Error {}

pub struct SourceFile {
    document: Document,
    observation: ReadObservation,
    sha256: Option<String>,
    #[cfg(unix)]
    permissions: Option<u32>,
}

impl SourceFile {
    pub fn document(&self) -> &Document {
        &self.document
    }
    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }
    #[cfg(unix)]
    pub fn permissions(&self) -> Option<u32> {
        self.permissions
    }
}

pub struct SourceCohort {
    reader: DirectoryReader,
    files: Vec<SourceFile>,
    unsupported_absences: Vec<ReadObservation>,
    total_bytes: usize,
    deadline: Instant,
    cancelled: Arc<AtomicBool>,
}

impl SourceCohort {
    pub fn inspect(
        path: impl AsRef<Path>,
        include_aliases: bool,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Self, Error> {
        Self::inspect_before(
            path.as_ref(),
            include_aliases,
            cancelled,
            Instant::now() + Duration::from_secs(30),
        )
    }

    fn inspect_before(
        path: &Path,
        include_aliases: bool,
        cancelled: Arc<AtomicBool>,
        deadline: Instant,
    ) -> Result<Self, Error> {
        let check = || check_progress(deadline, &cancelled);
        check()?;
        let reader = DirectoryReader::open_existing(path)
            .map_err(|_| error(None, Reason::SourceUnavailable))?;
        let mut unsupported_absences = Vec::with_capacity(UNSUPPORTED_SOURCE_ENTRIES.len());
        for name in UNSUPPORTED_SOURCE_ENTRIES {
            check()?;
            let observation = reader
                .observe(name, 1)
                .map_err(|_| error(None, Reason::UnsupportedSourceGeneration))?;
            if observation.bytes().is_some() {
                return Err(error(None, Reason::UnsupportedSourceGeneration));
            }
            unsupported_absences.push(observation);
        }
        let mut files = Vec::with_capacity(SOURCE_ROLES.len() + usize::from(include_aliases));
        let mut total_bytes: usize = 0;
        for role in SOURCE_ROLES
            .into_iter()
            .chain(include_aliases.then_some(Role::Aliases))
        {
            check()?;
            let observation = reader
                .observe(role.filename(), MAX_DOCUMENT_BYTES as u64)
                .map_err(|_| error(Some(role), Reason::SourceUnavailable))?;
            total_bytes = total_bytes
                .checked_add(observation.bytes().map_or(0, <[u8]>::len))
                .filter(|bytes| *bytes <= MAX_COHORT_BYTES)
                .ok_or_else(|| error(Some(role), Reason::CohortTooLarge))?;
            let sha256 = observation
                .bytes()
                .map(|bytes| format!("{:x}", Sha256::digest(bytes)));
            #[cfg(unix)]
            let permissions = {
                use std::os::unix::fs::PermissionsExt;
                observation
                    .permissions()
                    .map_err(|_| error(Some(role), Reason::SourceUnavailable))?
                    .map(|permissions| permissions.mode() & 0o7777)
            };
            let document = Document::parse(role, observation.shared_bytes())
                .map_err(|failure| error(Some(role), Reason::Document(failure.reason)))?;
            files.push(SourceFile {
                document,
                observation,
                sha256,
                #[cfg(unix)]
                permissions,
            });
        }
        let cohort = Self {
            reader,
            files,
            unsupported_absences,
            total_bytes,
            deadline,
            cancelled,
        };
        cohort.verify()?;
        Ok(cohort)
    }

    pub fn file(&self, role: Role) -> Option<&SourceFile> {
        self.files.iter().find(|file| file.document.role() == role)
    }
    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }
    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    pub fn verify(&self) -> Result<(), Error> {
        check_progress(self.deadline, &self.cancelled)?;
        self.reader
            .verify()
            .map_err(|_| error(None, Reason::SourceUnavailable))?;
        for absence in &self.unsupported_absences {
            check_progress(self.deadline, &self.cancelled)?;
            absence
                .verify()
                .map_err(|_| error(None, Reason::UnsupportedSourceGeneration))?;
        }
        for file in &self.files {
            check_progress(self.deadline, &self.cancelled)?;
            file.observation
                .verify()
                .map_err(|_| error(Some(file.document.role()), Reason::SourceUnavailable))?;
        }
        check_progress(self.deadline, &self.cancelled)
    }
}

fn error(role: Option<Role>, reason: Reason) -> Error {
    Error { role, reason }
}

fn check_progress(deadline: Instant, cancelled: &AtomicBool) -> Result<(), Error> {
    if cancelled.load(Ordering::Acquire) {
        return Err(error(None, Reason::Cancelled));
    }
    if Instant::now() >= deadline {
        return Err(error(None, Reason::Deadline));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn cancelled() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    #[test]
    fn copied_source_keeps_exact_bytes_missing_roles_permissions_and_shared_buffers_without_writes()
    {
        let source = tempfile::tempdir().unwrap();
        let bytes = b"{ \"unknown\" : 123456789012345678901234567890 }\n";
        fs::write(source.path().join("recipe_options.json"), bytes).unwrap();
        fs::write(source.path().join("aliases.json"), b"{\"alias\":\"value\"}").unwrap();
        let cohort = SourceCohort::inspect(source.path(), false, cancelled()).unwrap();
        assert_eq!(cohort.total_bytes(), bytes.len());
        assert_eq!(cohort.files().len(), 6);
        assert!(cohort.file(Role::Aliases).is_none());
        assert!(cohort.file(Role::Config).unwrap().document().is_absent());
        let file = cohort.file(Role::RecipeOptions).unwrap();
        assert_eq!(file.document().original_bytes().unwrap().as_slice(), bytes);
        assert_eq!(
            file.sha256(),
            Some(format!("{:x}", Sha256::digest(bytes)).as_str())
        );
        assert!(Arc::ptr_eq(
            file.document().original_bytes().unwrap(),
            &file.observation.shared_bytes().unwrap()
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                file.permissions(),
                Some(
                    fs::metadata(source.path().join("recipe_options.json"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o7777
                )
            );
        }
        cohort.verify().unwrap();
        assert_eq!(fs::read_dir(source.path()).unwrap().count(), 2);
        assert_eq!(
            fs::read(source.path().join("recipe_options.json")).unwrap(),
            bytes
        );
        let with_aliases = SourceCohort::inspect(source.path(), true, cancelled()).unwrap();
        assert_eq!(with_aliases.files().len(), 7);
        assert!(
            !with_aliases
                .file(Role::Aliases)
                .unwrap()
                .document()
                .is_absent()
        );
    }

    #[test]
    fn malformed_present_documents_are_redacted_and_never_reset_to_missing() {
        let source = tempfile::tempdir().unwrap();
        for bytes in [
            b"\"PRIVATE_SOURCE_VALUE\"".as_slice(),
            b"{\"PRIVATE_SOURCE_VALUE\":1,\"PRIVATE_SOURCE_VALUE\":2}",
            b"{\"key\":PRIVATE_SOURCE_VALUE}",
        ] {
            fs::write(source.path().join("user_models.json"), bytes).unwrap();
            let failure = SourceCohort::inspect(source.path(), false, cancelled())
                .err()
                .unwrap();
            assert_eq!(failure.role, Some(Role::UserModels));
            assert!(matches!(failure.reason, Reason::Document(_)));
            assert!(!format!("{failure} {failure:?}").contains("PRIVATE_SOURCE_VALUE"));
            assert!(std::error::Error::source(&failure).is_none());
            assert_eq!(
                fs::read(source.path().join("user_models.json")).unwrap(),
                bytes
            );
        }
        assert_eq!(fs::read_dir(source.path()).unwrap().count(), 1);
    }

    #[test]
    fn source_changes_absence_changes_and_modern_state_block_further_use() {
        let source = tempfile::tempdir().unwrap();
        fs::write(source.path().join("jobs.json"), b"{}").unwrap();
        let cohort = SourceCohort::inspect(source.path(), false, cancelled()).unwrap();
        fs::write(source.path().join("jobs.json"), b"{\"changed\":true}").unwrap();
        assert_eq!(cohort.verify().unwrap_err().role, Some(Role::Jobs));
        let cohort = SourceCohort::inspect(source.path(), false, cancelled()).unwrap();
        fs::write(source.path().join("native_helpers.json"), b"{}").unwrap();
        assert_eq!(cohort.verify().unwrap_err().role, Some(Role::NativeHelpers));
        for name in UNSUPPORTED_SOURCE_ENTRIES {
            let cohort = SourceCohort::inspect(source.path(), false, cancelled()).unwrap();
            fs::create_dir(source.path().join(name)).unwrap();
            assert_eq!(
                cohort.verify().unwrap_err().reason,
                Reason::UnsupportedSourceGeneration
            );
            assert_eq!(
                SourceCohort::inspect(source.path(), false, cancelled())
                    .err()
                    .unwrap()
                    .reason,
                Reason::UnsupportedSourceGeneration
            );
            fs::remove_dir(source.path().join(name)).unwrap();
        }
    }

    #[test]
    fn full_size_cohort_is_readable_but_one_more_document_exceeds_the_aggregate_bound() {
        let source = tempfile::tempdir().unwrap();
        let mut bytes = Vec::with_capacity(MAX_DOCUMENT_BYTES);
        bytes.extend_from_slice(b"{\"padding\":\"");
        bytes.resize(MAX_DOCUMENT_BYTES - 2, b'x');
        bytes.extend_from_slice(b"\"}");
        for role in SOURCE_ROLES.into_iter().take(4) {
            std::fs::write(source.path().join(role.filename()), &bytes).unwrap();
        }
        {
            let cohort = SourceCohort::inspect(source.path(), false, cancelled()).unwrap();
            assert_eq!(cohort.total_bytes(), MAX_COHORT_BYTES);
            cohort.verify().unwrap();
        }
        std::fs::write(
            source.path().join(Role::ExecutionProfiles.filename()),
            b"{}",
        )
        .unwrap();
        let failure = SourceCohort::inspect(source.path(), false, cancelled())
            .err()
            .unwrap();
        assert_eq!(failure.reason, Reason::CohortTooLarge);
        assert_eq!(failure.role, Some(Role::ExecutionProfiles));
        assert_eq!(std::fs::read_dir(source.path()).unwrap().count(), 5);
        assert_eq!(
            std::fs::read(source.path().join(Role::Config.filename())).unwrap(),
            bytes
        );
    }

    #[test]
    fn cancellation_deadline_and_missing_root_do_not_create_source_paths() {
        let source = tempfile::tempdir().unwrap();
        let cancel = cancelled();
        let cohort = SourceCohort::inspect(source.path(), false, cancel.clone()).unwrap();
        cancel.store(true, Ordering::Release);
        assert_eq!(cohort.verify().unwrap_err().reason, Reason::Cancelled);
        assert_eq!(
            SourceCohort::inspect(source.path(), false, cancel)
                .err()
                .unwrap()
                .reason,
            Reason::Cancelled
        );
        assert_eq!(
            SourceCohort::inspect_before(source.path(), false, cancelled(), Instant::now())
                .err()
                .unwrap()
                .reason,
            Reason::Deadline
        );
        assert_eq!(
            SourceCohort::inspect(source.path().join("missing"), false, cancelled())
                .err()
                .unwrap()
                .reason,
            Reason::SourceUnavailable
        );
        assert_eq!(fs::read_dir(source.path()).unwrap().count(), 0);
    }
}
