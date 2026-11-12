pub mod admission;
pub mod api_keys;
pub mod artifacts;
pub mod audio;
pub mod auth;
mod bounded_json;
pub mod cache;
pub mod catalog;
pub mod compat;
pub mod config;
pub mod connect;
pub mod discovery;
pub mod engine_usage;
pub mod engines;
pub mod execution_profile;
pub mod extract;
pub mod flm;
pub mod hardware;
pub mod host_memory;
pub mod http;
pub mod hybrid;
pub mod identity;
pub mod library;
pub mod logs;
pub mod media;
pub mod native_execution;
pub mod native_import;
pub mod native_settings;
pub mod native_store;
pub mod power;
pub mod process;
pub mod profiles;
pub mod profiling;
pub mod protocol_access;
pub mod protocols;
pub mod realtime;
pub mod resource_profile;
pub mod runtime_arguments;
pub mod runtime_bundle;
pub mod runtime_identity;
pub mod runtime_inputs;
pub mod runtime_packages;
pub mod sessions;
pub mod settings_backup;
pub mod speech;
pub mod startup;
pub mod storage;
pub mod supervisor;
pub mod system_stats;
pub mod voice;
pub mod writer_identity;
pub mod wyoming;

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde_json::json;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApiError {
    pub fn new(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        (self.status, Json(json!({"error": {"code": self.code, "message": self.message, "type": "juiced_error"}}))).into_response()
    }
}

impl From<crate::artifacts::ArtifactError> for ApiError {
    fn from(error: crate::artifacts::ArtifactError) -> Self {
        use crate::artifacts::ArtifactError as E;
        let status = match error {
            E::RootUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            E::Deadline => StatusCode::GATEWAY_TIMEOUT,
            E::SetBinding => StatusCode::CONFLICT,
            E::SetUnsupported => StatusCode::NOT_IMPLEMENTED,
            E::Invalid => StatusCode::BAD_REQUEST,
            E::NotFound => StatusCode::NOT_FOUND,
            E::Conflict | E::Busy | E::SourceChanged => StatusCode::CONFLICT,
            E::Offline | E::SourcePolicy => StatusCode::FORBIDDEN,
            E::Limit => StatusCode::INSUFFICIENT_STORAGE,
            E::Network | E::HashMismatch | E::SizeMismatch => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError::new(status, error.code(), error.public_message())
    }
}

impl From<admission::Error> for ApiError {
    fn from(error: admission::Error) -> Self {
        Self::new(
            StatusCode::from_u16(error.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            error.code,
            error.message,
        )
    }
}

/// The release version, set at build time for published images; the crate version otherwise.
pub const VERSION: &str = match option_env!("JUICED_SERVICE_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => env!("CARGO_PKG_VERSION"),
};
