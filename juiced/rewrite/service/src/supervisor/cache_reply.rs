use super::{ApiError, Arc, state::CacheActivity};
use axum::response::Response;
use serde::Serialize;

pub struct CacheReply<T> {
    value: T,
    activity: Arc<CacheActivity>,
    maximum_bytes: u64,
}

impl<T: Serialize + Send + 'static> CacheReply<T> {
    pub(super) fn new(value: T, activity: Arc<CacheActivity>) -> Self {
        let maximum_bytes = activity.response_limit();
        Self {
            value,
            activity,
            maximum_bytes,
        }
    }

    pub async fn into_response(self) -> Result<Response, ApiError> {
        crate::bounded_json::respond(
            self.value,
            self.activity,
            self.maximum_bytes,
            crate::bounded_json::Domain::Cache,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::supervisor::state::Operations;
    use futures_util::StreamExt;
    use std::{sync::Mutex, time::Duration};

    #[tokio::test]
    async fn transport_bytes_and_slices_retain_activity_after_body_and_reply_drop() {
        let operations = Arc::new(Operations::default());
        let response = CacheReply::new(serde_json::json!({"saved":true}), operations.enter(false))
            .into_response()
            .await
            .unwrap();
        assert_eq!(operations.active(), 1);
        let mut body = response.into_body().into_data_stream();
        let bytes = body.next().await.unwrap().unwrap();
        let retained = bytes.slice(1..);
        drop(body);
        drop(bytes);
        assert_eq!(operations.active(), 1);
        assert!(!operations.drain(tokio::time::Instant::now()).await);
        drop(retained);
        assert!(
            operations
                .drain(tokio::time::Instant::now() + Duration::from_secs(1))
                .await
        );
        assert_eq!(operations.active(), 0);
    }

    #[tokio::test]
    async fn serialization_refuses_oversized_output_and_releases_its_owner() {
        let operations = Arc::new(Operations::default());
        let mut reply = CacheReply::new("x".repeat(128), operations.enter(false));
        reply.maximum_bytes = 64;
        let error = reply.into_response().await.unwrap_err();
        assert_eq!(error.code, "cache_response_limit");
        assert_eq!(operations.active(), 0);
    }

    #[tokio::test]
    async fn cancelled_serialization_keeps_actual_blocking_work_owned_until_it_finishes() {
        struct Probe {
            started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
            resume: Mutex<std::sync::mpsc::Receiver<()>>,
        }
        impl Serialize for Probe {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                if let Some(started) = self.started.lock().unwrap().take() {
                    let _ = started.send(());
                    self.resume
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(5))
                        .map_err(serde::ser::Error::custom)?;
                }
                serializer.serialize_bool(true)
            }
        }
        let operations = Arc::new(Operations::default());
        let (started, entered) = tokio::sync::oneshot::channel();
        let (resume, waiting) = std::sync::mpsc::channel();
        let reply = CacheReply::new(
            Probe {
                started: Mutex::new(Some(started)),
                resume: Mutex::new(waiting),
            },
            operations.enter(false),
        );
        let task = tokio::spawn(reply.into_response());
        entered.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(operations.active(), 1);
        assert!(!operations.drain(tokio::time::Instant::now()).await);
        resume.send(()).unwrap();
        assert!(
            operations
                .drain(tokio::time::Instant::now() + Duration::from_secs(2))
                .await
        );
        assert_eq!(operations.active(), 0);
    }
}
