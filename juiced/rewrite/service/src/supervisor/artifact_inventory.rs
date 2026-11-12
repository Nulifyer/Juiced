use super::*;
use crate::artifacts::{InventoryRequest, inventory_bounds};

impl Manager {
    pub async fn artifact_inventory(
        &self,
        request: InventoryRequest,
    ) -> Result<axum::response::Response, ApiError> {
        let store = self.artifacts()?;
        let claim = if self.admission.memory_protected() {
            self.host_memory.refresh(&self.admission).await?;
            Some(self.admission.reserve_control_host(
                inventory_bounds::RESPONSE_WORK_BYTES,
                crate::admission::Priority::Interactive,
            )?)
        } else {
            None
        };
        let (page, claim) = tokio::task::spawn_blocking(move || {
            store.inventory(&request).map(|page| (page, claim))
        })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "artifact_operation_interrupted",
                "The artifact operation was interrupted. Inspect its current state before retrying.",
            )
        })??;
        crate::bounded_json::respond(
            page,
            claim,
            inventory_bounds::RESPONSE_BYTES,
            crate::bounded_json::Domain::Artifact,
        )
        .await
    }
}
