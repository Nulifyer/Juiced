use crate::{
    ApiError,
    extract::{ApiJson, ApiQuery},
    http::AppState,
    supervisor::Reclaim,
};
use axum::{
    Json,
    extract::State,
    http::{StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StorageQuery {
    #[serde(default)]
    summary: Option<String>,
}

pub(crate) async fn storage_view(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<StorageQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let summary = matches!(query.summary.as_deref(), Some("1" | "true"));
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(state.manager.storage_view(summary).await?),
    ))
}

pub(crate) async fn storage_reclaim(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<Reclaim>,
) -> Result<Json<Value>, ApiError> {
    // A client disconnect must not separate a removal's reference check from its effect.
    tokio::spawn(async move { state.manager.storage_reclaim(request).await.map(Json) })
        .await
        .map_err(|_| {
            ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "storage_reclaim_interrupted",
                "Reclaiming was interrupted. Refresh the storage view before retrying.",
            )
        })?
}
