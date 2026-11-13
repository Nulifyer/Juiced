//! Values that must reach the operator but never the model: created API key secrets and test
//! media. Tool results carry an opaque handle; the console redeems it through an operator route.

use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

const SECRET_TTL: Duration = Duration::from_secs(10 * 60);
const MEDIA_TTL: Duration = Duration::from_secs(30 * 60);
const MAXIMUM_ENTRIES: usize = 32;
const MAXIMUM_MEDIA_BYTES: usize = 48 * 1024 * 1024;

pub(super) enum Value {
    Secret(String),
    Media {
        content_type: String,
        bytes: Vec<u8>,
    },
}

struct Entry {
    value: Value,
    created: Instant,
}

impl Entry {
    fn expired(&self, now: Instant) -> bool {
        let ttl = match self.value {
            Value::Secret(_) => SECRET_TTL,
            Value::Media { .. } => MEDIA_TTL,
        };
        now.duration_since(self.created) > ttl
    }
    fn bytes(&self) -> usize {
        match &self.value {
            Value::Secret(secret) => secret.len(),
            Value::Media { bytes, .. } => bytes.len(),
        }
    }
}

static STORE: LazyLock<Mutex<HashMap<String, Entry>>> = LazyLock::new(Mutex::default);

pub(super) fn store(value: Value) -> Option<String> {
    let id = format!("h_{}", crate::storage::generated_id().ok()?);
    let mut store = STORE.lock().unwrap_or_else(|error| error.into_inner());
    let now = Instant::now();
    store.retain(|_, entry| !entry.expired(now));
    let incoming = match &value {
        Value::Secret(secret) => secret.len(),
        Value::Media { bytes, .. } => bytes.len(),
    };
    while !store.is_empty()
        && (store.len() >= MAXIMUM_ENTRIES
            || store.values().map(Entry::bytes).sum::<usize>() + incoming > MAXIMUM_MEDIA_BYTES)
    {
        let oldest = store
            .iter()
            .min_by_key(|(_, entry)| entry.created)
            .map(|(key, _)| key.clone())?;
        store.remove(&oldest);
    }
    store.insert(
        id.clone(),
        Entry {
            value,
            created: now,
        },
    );
    Some(id)
}

/// Secrets are released once; media stays until it expires so a player can fetch it again.
pub(super) fn redeem(id: &str) -> Option<Value> {
    let mut store = STORE.lock().unwrap_or_else(|error| error.into_inner());
    let now = Instant::now();
    store.retain(|_, entry| !entry.expired(now));
    match &store.get(id)?.value {
        Value::Secret(_) => store.remove(id).map(|entry| entry.value),
        Value::Media {
            content_type,
            bytes,
        } => Some(Value::Media {
            content_type: content_type.clone(),
            bytes: bytes.clone(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_redeem_once_and_media_repeatedly() {
        let secret = store(Value::Secret("juiced_example".into())).unwrap();
        assert!(matches!(redeem(&secret), Some(Value::Secret(value)) if value == "juiced_example"));
        assert!(redeem(&secret).is_none());
        let media = store(Value::Media {
            content_type: "audio/wav".into(),
            bytes: vec![1, 2, 3],
        })
        .unwrap();
        assert!(matches!(redeem(&media), Some(Value::Media { .. })));
        assert!(matches!(redeem(&media), Some(Value::Media { bytes, .. }) if bytes == [1, 2, 3]));
        assert!(redeem("h_missing").is_none());
    }
}
