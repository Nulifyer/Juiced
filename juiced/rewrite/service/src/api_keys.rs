use crate::ApiError;
use axum::http::StatusCode;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{
        Arc, Mutex, RwLock, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use subtle::ConstantTimeEq;

pub const FILENAME: &str = "api-keys.json";
pub const MAXIMUM_KEYS: usize = 64;
pub const MAXIMUM_NAME_CHARS: usize = 64;
pub const MAXIMUM_EXPIRY_DAYS: u32 = 3650;
pub const LAST_USED_FLUSH: Duration = Duration::from_secs(60);
const MAXIMUM_FILE_BYTES: u64 = 256 * 1024;
const TOKEN_PREFIX: &str = "juiced_";
const SHOWN_PREFIX_CHARS: usize = TOKEN_PREFIX.len() + 6;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    Read,
    Inference,
    Operator,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Stored {
    id: String,
    name: String,
    scope: Scope,
    prefix: String,
    sha256: String,
    created_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_used_at_ms: Option<u64>,
}

impl Stored {
    fn expired(&self, now: u64) -> bool {
        self.expires_at_ms.is_some_and(|at| at <= now)
    }

    fn anchors_operators(&self) -> bool {
        self.scope == Scope::Operator && self.expires_at_ms.is_none()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Image {
    schema_version: u32,
    keys: Vec<Stored>,
}

struct Live {
    stored: Stored,
    digest: [u8; 32],
    epoch: Arc<str>,
}

impl Live {
    fn new(stored: Stored) -> Result<Self, String> {
        let digest = decode_digest(&stored.sha256).ok_or("a key hash is not SHA-256 hex")?;
        if !valid_name(&stored.name) || stored.id.is_empty() || stored.id.len() > 64 {
            return Err("a key name or ID is invalid".into());
        }
        let epoch = format!(
            "{:x}",
            Sha256::digest(format!(
                "juiced-auth-v1\0issued\0{}\0{}",
                stored.id, stored.sha256
            ))
        )
        .into();
        Ok(Self {
            stored,
            digest,
            epoch,
        })
    }
}

#[derive(Clone)]
enum Backing {
    Memory,
    Directory {
        directory: PathBuf,
        lease: Option<crate::storage::DirectoryLease>,
    },
}

pub struct Match {
    pub id: String,
    pub scope: Scope,
    pub epoch: Arc<str>,
}

pub enum Lookup {
    Unknown,
    Expired,
    Found(Match),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Create {
    pub name: String,
    pub scope: Scope,
    #[serde(default)]
    pub expires_in_days: Option<u32>,
}

pub struct KeyStore {
    backing: Backing,
    keys: RwLock<Arc<Vec<Live>>>,
    unavailable: Option<String>,
    last_used: Mutex<HashMap<String, u64>>,
    dirty: AtomicBool,
    writer: tokio::sync::Mutex<()>,
}

impl KeyStore {
    pub fn memory() -> Arc<Self> {
        Arc::new(Self::with(Backing::Memory, Vec::new(), None))
    }

    /// An unreadable store keeps enforcement on: falling back to trusted-LAN access would open
    /// a server that an operator had protected.
    pub fn open(directory: PathBuf, lease: Option<crate::storage::DirectoryLease>) -> Arc<Self> {
        let bytes = match &lease {
            Some(lease) => lease.read_bytes(FILENAME, MAXIMUM_FILE_BYTES),
            None => std::fs::read(directory.join(FILENAME)),
        };
        let (keys, unavailable) = match bytes {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (Vec::new(), None),
            Err(error) => (Vec::new(), Some(format!("cannot be read ({error})"))),
            Ok(bytes) => match parse(&bytes) {
                Ok(keys) => (keys, None),
                Err(message) => (Vec::new(), Some(message)),
            },
        };
        Arc::new(Self::with(
            Backing::Directory { directory, lease },
            keys,
            unavailable,
        ))
    }

    fn with(backing: Backing, keys: Vec<Live>, unavailable: Option<String>) -> Self {
        Self {
            backing,
            keys: RwLock::new(Arc::new(keys)),
            unavailable,
            last_used: Mutex::new(HashMap::new()),
            dirty: AtomicBool::new(false),
            writer: tokio::sync::Mutex::new(()),
        }
    }

    fn snapshot(&self) -> Arc<Vec<Live>> {
        self.keys
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub fn unavailable(&self) -> Option<&str> {
        self.unavailable.as_deref()
    }

    pub fn enforcing(&self) -> bool {
        self.unavailable.is_some() || !self.snapshot().is_empty()
    }

    pub fn lookup(&self, token: &str) -> Lookup {
        if !token.starts_with(TOKEN_PREFIX) {
            return Lookup::Unknown;
        }
        let digest: [u8; 32] = Sha256::digest(token.as_bytes()).into();
        let keys = self.snapshot();
        let Some(key) = keys
            .iter()
            .find(|key| bool::from(key.digest.ct_eq(&digest)))
        else {
            return Lookup::Unknown;
        };
        let now = now_ms();
        if key.stored.expired(now) {
            return Lookup::Expired;
        }
        self.last_used
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(key.stored.id.clone(), now);
        self.dirty.store(true, Ordering::Release);
        Lookup::Found(Match {
            id: key.stored.id.clone(),
            scope: key.stored.scope,
            epoch: key.epoch.clone(),
        })
    }

    pub fn views(&self) -> Vec<Value> {
        let now = now_ms();
        let used = self
            .last_used
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        self.snapshot()
            .iter()
            .map(|key| view(&key.stored, used.get(&key.stored.id).copied(), now))
            .collect()
    }

    pub async fn create(
        self: &Arc<Self>,
        request: Create,
        environment_operator: bool,
    ) -> Result<(Value, String), ApiError> {
        self.writable()?;
        let name = request.name.trim().to_owned();
        if !valid_name(&name) {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_key_name",
                format!("Name the key with 1 to {MAXIMUM_NAME_CHARS} printable characters."),
            ));
        }
        if request
            .expires_in_days
            .is_some_and(|days| days == 0 || days > MAXIMUM_EXPIRY_DAYS)
        {
            return Err(ApiError::new(
                StatusCode::BAD_REQUEST,
                "invalid_key_expiry",
                format!("Choose an expiry from 1 to {MAXIMUM_EXPIRY_DAYS} days, or none."),
            ));
        }
        let _writer = self.writer.lock().await;
        let current = self.snapshot();
        if current.len() >= MAXIMUM_KEYS {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "key_limit",
                format!("Up to {MAXIMUM_KEYS} keys can be issued. Revoke one first."),
            ));
        }
        if current
            .iter()
            .any(|key| key.stored.name.eq_ignore_ascii_case(&name))
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "key_name_taken",
                "Another key already uses this name.",
            ));
        }
        let anchored =
            environment_operator || current.iter().any(|key| key.stored.anchors_operators());
        if !anchored && (request.scope != Scope::Operator || request.expires_in_days.is_some()) {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "operator_key_first",
                "Create an operator key without expiry first. Keys are enforced once one exists, and operators need a key that keeps working.",
            ));
        }
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(|_| random_failure())?;
        let token = format!(
            "{TOKEN_PREFIX}{}",
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret)
        );
        let mut id = crate::storage::generated_id().map_err(|_| random_failure())?;
        id.truncate(12);
        let now = now_ms();
        let stored = Stored {
            id: format!("key_{id}"),
            name,
            scope: request.scope,
            prefix: token[..SHOWN_PREFIX_CHARS].to_owned(),
            sha256: format!("{:x}", Sha256::digest(token.as_bytes())),
            created_at_ms: now,
            expires_at_ms: request
                .expires_in_days
                .map(|days| now.saturating_add(u64::from(days) * 86_400_000)),
            last_used_at_ms: None,
        };
        let created = view(&stored, None, now);
        let mut next = self.merged(&current);
        next.push(stored);
        self.commit(next).await?;
        Ok((created, token))
    }

    pub async fn revoke(
        self: &Arc<Self>,
        id: &str,
        environment_operator: bool,
    ) -> Result<(), ApiError> {
        self.writable()?;
        let _writer = self.writer.lock().await;
        let current = self.snapshot();
        let Some(target) = current.iter().find(|key| key.stored.id == id) else {
            return Err(ApiError::new(
                StatusCode::NOT_FOUND,
                "api_key_not_found",
                "No issued key has this ID. Refresh the list.",
            ));
        };
        let other_anchor = current
            .iter()
            .any(|key| key.stored.id != id && key.stored.anchors_operators());
        if target.stored.anchors_operators()
            && !environment_operator
            && !other_anchor
            && current.len() > 1
        {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "last_operator_key",
                "This is the last operator key without expiry. Create another operator key or revoke the other keys first.",
            ));
        }
        let next = self
            .merged(&current)
            .into_iter()
            .filter(|key| key.id != id)
            .collect();
        self.commit(next).await
    }

    pub async fn flush(self: &Arc<Self>) -> Result<(), ApiError> {
        if self.unavailable.is_some() || !self.dirty.load(Ordering::Acquire) {
            return Ok(());
        }
        let _writer = self.writer.lock().await;
        let current = self.snapshot();
        self.commit(self.merged(&current)).await
    }

    pub fn spawn_flusher(self: &Arc<Self>) {
        if matches!(self.backing, Backing::Memory) {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let store: Weak<Self> = Arc::downgrade(self);
        runtime.spawn(async move {
            let mut ticks = tokio::time::interval(LAST_USED_FLUSH);
            ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            ticks.tick().await;
            loop {
                ticks.tick().await;
                let Some(store) = store.upgrade() else {
                    break;
                };
                let _ = store.flush().await;
            }
        });
    }

    fn writable(&self) -> Result<(), ApiError> {
        match &self.unavailable {
            Some(reason) => Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "key_store_unavailable",
                format!("The API key store {reason}. Fix or remove {FILENAME}, then restart."),
            )),
            None => Ok(()),
        }
    }

    fn merged(&self, current: &[Live]) -> Vec<Stored> {
        let used = self
            .last_used
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        current
            .iter()
            .map(|key| {
                let mut stored = key.stored.clone();
                stored.last_used_at_ms = stored.last_used_at_ms.max(used.get(&stored.id).copied());
                stored
            })
            .collect()
    }

    async fn commit(self: &Arc<Self>, next: Vec<Stored>) -> Result<(), ApiError> {
        self.dirty.store(false, Ordering::Release);
        let backing = self.backing.clone();
        let image = Image {
            schema_version: 1,
            keys: next.clone(),
        };
        let written = tokio::task::spawn_blocking(move || write(&backing, &image))
            .await
            .unwrap_or(Err(()));
        if written.is_err() {
            self.dirty.store(true, Ordering::Release);
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "key_store_write_failed",
                format!(
                    "The API key store could not be saved. Check that {FILENAME} in the configuration directory is writable."
                ),
            ));
        }
        let live = next
            .into_iter()
            .map(Live::new)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| random_failure())?;
        *self.keys.write().unwrap_or_else(|error| error.into_inner()) = Arc::new(live);
        Ok(())
    }
}

fn write(backing: &Backing, image: &Image) -> Result<(), ()> {
    let result = match backing {
        Backing::Memory => return Ok(()),
        Backing::Directory {
            lease: Some(lease), ..
        } => lease.write_json(FILENAME, image),
        Backing::Directory { directory, .. } => {
            crate::storage::atomic_json(&directory.join(FILENAME), image)
        }
    };
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.committed() => Ok(()),
        Err(_) => Err(()),
    }
}

fn parse(bytes: &[u8]) -> Result<Vec<Live>, String> {
    let image: Image =
        serde_json::from_slice(bytes).map_err(|error| format!("is not valid ({error})"))?;
    if image.schema_version != 1 {
        return Err("has an unsupported schema version".into());
    }
    if image.keys.len() > MAXIMUM_KEYS {
        return Err("holds more keys than supported".into());
    }
    let keys = image
        .keys
        .into_iter()
        .map(Live::new)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|message| format!("is not valid ({message})"))?;
    let mut ids: Vec<_> = keys.iter().map(|key| key.stored.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.len() != keys.len() {
        return Err("is not valid (duplicate key IDs)".into());
    }
    Ok(keys)
}

fn view(stored: &Stored, used: Option<u64>, now: u64) -> Value {
    json!({
        "id": stored.id, "name": stored.name, "scope": stored.scope, "source": "issued",
        "prefix": stored.prefix, "created_at_ms": stored.created_at_ms,
        "expires_at_ms": stored.expires_at_ms, "expired": stored.expired(now),
        "last_used_at_ms": stored.last_used_at_ms.max(used),
    })
}

fn valid_name(name: &str) -> bool {
    let count = name.chars().count();
    (1..=MAXIMUM_NAME_CHARS).contains(&count)
        && !name.chars().any(char::is_control)
        && name.trim() == name
}

fn decode_digest(hex: &str) -> Option<[u8; 32]> {
    if hex.len() != 64 {
        return None;
    }
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(hex.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    hex.bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        .then_some(digest)
}

fn random_failure() -> ApiError {
    ApiError::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        "key_generation_failed",
        "A key could not be generated. Retry.",
    )
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create(name: &str, scope: Scope, days: Option<u32>) -> Create {
        Create {
            name: name.into(),
            scope,
            expires_in_days: days,
        }
    }

    #[tokio::test]
    async fn the_first_key_must_be_a_lasting_operator_key() {
        let store = KeyStore::memory();
        assert!(!store.enforcing());
        for request in [
            create("client", Scope::Inference, None),
            create("short", Scope::Operator, Some(1)),
        ] {
            assert_eq!(
                store.create(request, false).await.unwrap_err().code,
                "operator_key_first"
            );
        }
        assert!(
            store
                .create(create("env backed", Scope::Read, Some(3)), true)
                .await
                .is_ok()
        );
        let store = KeyStore::memory();
        let (view, secret) = store
            .create(create("console", Scope::Operator, None), false)
            .await
            .unwrap();
        assert!(store.enforcing());
        assert!(secret.starts_with("juiced_") && secret.len() == 50);
        assert_eq!(view["prefix"], &secret[..13]);
        assert!(!view.to_string().contains(&secret));
        assert!(
            matches!(store.lookup(&secret), Lookup::Found(found) if found.scope == Scope::Operator)
        );
        assert!(matches!(store.lookup("juiced_wrong"), Lookup::Unknown));
    }

    #[tokio::test]
    async fn revocation_protects_the_last_operator_anchor() {
        let store = KeyStore::memory();
        let (operator, _) = store
            .create(create("console", Scope::Operator, None), false)
            .await
            .unwrap();
        let (client, secret) = store
            .create(create("client", Scope::Inference, Some(30)), false)
            .await
            .unwrap();
        let operator = operator["id"].as_str().unwrap();
        assert_eq!(
            store.revoke(operator, false).await.unwrap_err().code,
            "last_operator_key"
        );
        store.revoke(operator, true).await.unwrap();
        store
            .revoke(client["id"].as_str().unwrap(), false)
            .await
            .unwrap();
        assert!(matches!(store.lookup(&secret), Lookup::Unknown));
        assert!(!store.enforcing());
        assert_eq!(
            store.revoke("key_missing", false).await.unwrap_err().code,
            "api_key_not_found"
        );
    }

    #[tokio::test]
    async fn validation_rejects_names_expiry_and_duplicates() {
        let store = KeyStore::memory();
        for (name, days, code) in [
            ("", None, "invalid_key_name"),
            ("bad\nname", None, "invalid_key_name"),
            (&*"x".repeat(65), None, "invalid_key_name"),
            ("ok", Some(0), "invalid_key_expiry"),
            ("ok", Some(MAXIMUM_EXPIRY_DAYS + 1), "invalid_key_expiry"),
        ] {
            assert_eq!(
                store
                    .create(create(name, Scope::Operator, days), true)
                    .await
                    .unwrap_err()
                    .code,
                code
            );
        }
        store
            .create(create("Console", Scope::Operator, None), true)
            .await
            .unwrap();
        assert_eq!(
            store
                .create(create("console", Scope::Read, None), true)
                .await
                .unwrap_err()
                .code,
            "key_name_taken"
        );
    }

    #[tokio::test]
    async fn persisted_keys_store_only_hashes_and_flush_last_use_lazily() {
        let directory = tempfile::tempdir().unwrap();
        let store = KeyStore::open(directory.path().into(), None);
        let (_, secret) = store
            .create(create("console", Scope::Operator, None), false)
            .await
            .unwrap();
        let path = directory.path().join(FILENAME);
        let saved = std::fs::read_to_string(&path).unwrap();
        assert!(!saved.contains(&secret) && !saved.contains("last_used_at_ms"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        assert!(matches!(store.lookup(&secret), Lookup::Found(_)));
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("last_used_at_ms")
        );
        assert!(store.views()[0]["last_used_at_ms"].is_u64());
        store.flush().await.unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("last_used_at_ms")
        );
        let reopened = KeyStore::open(directory.path().into(), None);
        assert!(matches!(reopened.lookup(&secret), Lookup::Found(_)));
        assert!(reopened.views()[0]["last_used_at_ms"].is_u64());
    }

    #[tokio::test]
    async fn expired_and_unreadable_stores_fail_closed() {
        let directory = tempfile::tempdir().unwrap();
        let store = KeyStore::open(directory.path().into(), None);
        store
            .create(create("console", Scope::Operator, None), false)
            .await
            .unwrap();
        let (_, secret) = store
            .create(create("short", Scope::Read, Some(1)), false)
            .await
            .unwrap();
        let path = directory.path().join(FILENAME);
        let mut image: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        image["keys"][1]["expires_at_ms"] = json!(1);
        std::fs::write(&path, image.to_string()).unwrap();
        let reopened = KeyStore::open(directory.path().into(), None);
        assert!(matches!(reopened.lookup(&secret), Lookup::Expired));
        assert_eq!(reopened.views()[1]["expired"], true);
        std::fs::write(&path, b"{not json").unwrap();
        let broken = KeyStore::open(directory.path().into(), None);
        assert!(broken.enforcing() && broken.unavailable().is_some());
        assert_eq!(
            broken
                .create(create("x", Scope::Operator, None), true)
                .await
                .unwrap_err()
                .code,
            "key_store_unavailable"
        );
    }
}
