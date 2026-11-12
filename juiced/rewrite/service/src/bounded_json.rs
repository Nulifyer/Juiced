use crate::ApiError;
use axum::{
    body::{Body, Bytes},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use std::io::{self, Write};

#[derive(Clone, Copy)]
pub(crate) enum Domain {
    Artifact,
    Cache,
    Runtime,
}
impl Domain {
    fn interrupted(self) -> ApiError {
        match self {
            Self::Artifact => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "artifact_response_interrupted",
                "Artifact response serialization was interrupted. Refresh the library before retrying.",
            ),
            Self::Cache => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache_response_interrupted",
                "Cache response serialization was interrupted. Refresh retained state before retrying.",
            ),
            Self::Runtime => ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "runtime_response_interrupted",
                "Runtime response serialization was interrupted. Refresh installed packages before retrying.",
            ),
        }
    }
    fn limit(self) -> ApiError {
        match self {
            Self::Artifact => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "artifact_response_limit",
                "Artifact metadata exceeds the bounded response size. Preserve stored files and request a smaller page.",
            ),
            Self::Cache => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "cache_response_limit",
                "Cache metadata exceeds the bounded response size. Inspect retained storage before retrying.",
            ),
            Self::Runtime => ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "runtime_response_limit",
                "Runtime metadata exceeds the bounded response size. Preserve its files and refresh installed packages before retrying.",
            ),
        }
    }
}

pub(crate) async fn respond<T: Serialize + Send + 'static, O: Send + 'static>(
    value: T,
    owner: O,
    maximum_bytes: u64,
    domain: Domain,
) -> Result<Response, ApiError> {
    tokio::task::spawn_blocking(move || serialize(value, owner, maximum_bytes, domain))
        .await
        .map_err(|_| domain.interrupted())?
}

fn serialize<T: Serialize, O: Send + 'static>(
    value: T,
    owner: O,
    maximum_bytes: u64,
    domain: Domain,
) -> Result<Response, ApiError> {
    let invalid = || domain.limit();
    let length = crate::storage::json_size(&value, maximum_bytes).map_err(|_| invalid())?;
    struct Exact {
        bytes: Vec<u8>,
        remaining: usize,
    }
    impl Write for Exact {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.remaining = self
                .remaining
                .checked_sub(bytes.len())
                .ok_or_else(|| io::Error::other("JSON response changed while encoding"))?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let length = usize::try_from(length).map_err(|_| invalid())?;
    let mut output = Exact {
        bytes: Vec::with_capacity(length),
        remaining: length,
    };
    serde_json::to_writer(&mut output, &value).map_err(|_| invalid())?;
    if output.remaining != 0 {
        return Err(invalid());
    }
    let bytes = output.bytes;
    let bytes = Bytes::from_owner(OwnedResponse {
        bytes,
        _owner: owner,
    });
    Ok((
        [(header::CONTENT_TYPE, "application/json")],
        Body::from(bytes),
    )
        .into_response())
}

struct OwnedResponse<O> {
    bytes: Vec<u8>,
    _owner: O,
}
impl<O> AsRef<[u8]> for OwnedResponse<O> {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
