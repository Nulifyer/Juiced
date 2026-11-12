use super::*;

pub(crate) struct Store;
#[derive(Debug)]
pub(crate) struct Lease;
impl Store {
    pub(crate) fn open_optional_accounted(
        root: crate::storage::DirectoryLease,
        scope: Arc<crate::storage::LiveRoots>,
        _: Arc<memory::Controller>,
    ) -> Result<Option<Arc<Self>>> {
        Self::open_optional(root, scope)
    }
    pub(crate) fn publication_uncertain(&self) -> bool {
        false
    }
    pub(crate) fn open_optional(
        _: crate::storage::DirectoryLease,
        _: Arc<crate::storage::LiveRoots>,
    ) -> Result<Option<Arc<Self>>> {
        Ok(None)
    }
    pub(crate) fn verify(&self) -> Result<()> {
        Err(Error::new("platform_unsupported"))
    }
    pub(crate) fn inventory(&self) -> Result<serde_json::Value> {
        Err(Error::new("platform_unsupported"))
    }
    pub(crate) fn hold(
        self: &Arc<Self>,
        _: &str,
        _: crate::config::Provider,
        _: std::time::Instant,
    ) -> Result<Arc<Lease>> {
        Err(Error::new("platform_unsupported"))
    }
}
impl Lease {
    pub(crate) fn id(&self) -> &str {
        unreachable!()
    }
    pub(crate) fn bundle(&self) -> &BundleSpec {
        unreachable!()
    }
    pub(crate) fn verify(&self) -> Result<()> {
        Err(Error::new("platform_unsupported"))
    }
}
