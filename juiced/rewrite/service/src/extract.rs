use crate::ApiError;
use axum::{
    Json,
    extract::{FromRequest, FromRequestParts, Query, Request},
    http::{StatusCode, request::Parts},
};
use serde::de::DeserializeOwned;

pub struct ApiJson<T>(pub T);
pub struct ApiBytes(pub axum::body::Bytes);
pub struct ApiMultipart(pub axum::extract::Multipart);

impl<S: Send + Sync> FromRequest<S> for ApiMultipart {
    type Rejection = ApiError;
    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::extract::Multipart::from_request(request, state)
            .await
            .map(Self)
            .map_err(|rejection| {
                ApiError::new(
                    rejection.status(),
                    "invalid_multipart",
                    "Provide multipart/form-data with one model and one audio file.",
                )
            })
    }
}

impl<S: Send + Sync> FromRequest<S> for ApiBytes {
    type Rejection = ApiError;
    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::body::Bytes::from_request(request, state)
            .await
            .map(Self)
            .map_err(|rejection| {
                ApiError::new(
                    rejection.status(),
                    "invalid_body",
                    "The body could not be read within this endpoint's size limit.",
                )
            })
    }
}

impl<S, T> FromRequest<S> for ApiJson<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        Json::<T>::from_request(request, state)
            .await
            .map(|Json(value)| Self(value))
            .map_err(|rejection| {
                let status = rejection.status();
                ApiError::new(
                    status,
                    "invalid_request",
                    if status == StatusCode::PAYLOAD_TOO_LARGE {
                        "The request exceeds the endpoint body limit."
                    } else {
                        "Provide valid JSON matching the documented fields and types."
                    },
                )
            })
    }
}

pub struct ApiQuery<T>(pub T);

impl<S, T> FromRequestParts<S> for ApiQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        Query::<T>::from_request_parts(parts, state)
            .await
            .map(|Query(value)| Self(value))
            .map_err(|_| {
                ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "invalid_query",
                    "Provide valid query fields matching this endpoint's contract.",
                )
            })
    }
}
