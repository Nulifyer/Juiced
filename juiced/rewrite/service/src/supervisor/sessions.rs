use super::state::{ClaimCell, StateAction};
use super::*;
use crate::sessions::{Key, KeyKind, Mode, Scope, Settings};
use tokio::sync::OwnedMutexGuard;

const SAVE_ATTEMPTS: u32 = 4;
const LISTED_SESSIONS: usize = 256;
const LINEAGE_FILE: &str = "session-lineage.json";
const LINEAGE_FILE_BYTES: u64 = 16 * 1024 * 1024;
const LINEAGE_STALE: usize = 64;
/// A rate read from fewer prompt tokens or state bytes than these is mostly fixed cost.
const RATED_PROMPT_TOKENS: u64 = 256;
const RATED_STATE_BYTES: u64 = 16 * 1024 * 1024;

/// The leading message digests and text blocks of each saved session. A snapshot identity
/// names only a conversation's last digest, so without these a request that repeats part of
/// a saved one, such as its system prompt, cannot find the state that already holds it. The
/// file is advisory: identity and execution checks still gate every restore, and the engine
/// reuses only matching tokens.
pub(super) struct Lineages {
    sessions: Mutex<std::collections::HashMap<String, Lineage>>,
    root: Option<crate::storage::DirectoryLease>,
}

#[derive(Clone)]
struct Lineage {
    key: Arc<Key>,
    single: bool,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LineageFile {
    schema_version: u32,
    sessions: std::collections::BTreeMap<String, crate::sessions::LineageRecord>,
}

impl Lineages {
    pub(super) fn load(root: Option<&crate::storage::DirectoryLease>) -> Self {
        let sessions = root
            .and_then(|root| root.read_bytes(LINEAGE_FILE, LINEAGE_FILE_BYTES).ok())
            .and_then(|bytes| serde_json::from_slice::<LineageFile>(&bytes).ok())
            .filter(|file| file.schema_version == 1)
            .map(|file| {
                file.sessions
                    .iter()
                    .filter_map(|(id, record)| {
                        Some((
                            id.clone(),
                            Lineage {
                                key: Arc::new(Key::from_record(record)?),
                                single: record.single,
                            },
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            sessions: Mutex::new(sessions),
            root: root.cloned(),
        }
    }

    /// Saved states that open like `key`, with the request bytes each shares with it.
    fn shared_with(&self, key: &Key) -> std::collections::HashMap<String, u64> {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter_map(|(id, saved)| {
                let bytes = key.shared_bytes(&saved.key);
                (bytes >= crate::sessions::MINIMUM_SHARED_BYTES).then(|| (id.clone(), bytes))
            })
            .collect()
    }

    fn single(&self, id: &str) -> bool {
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(id)
            .is_some_and(|saved| saved.single)
    }

    /// Saved single-request states that a state saved for `key` replaces.
    fn siblings(&self, key: &Key) -> std::collections::HashSet<String> {
        if key.kind != KeyKind::Prefix {
            return std::collections::HashSet::new();
        }
        self.sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .filter(|(_, saved)| saved.single && key.replaces_sibling(&saved.key))
            .map(|(id, _)| id.clone())
            .collect()
    }

    fn record(
        &self,
        id: &str,
        key: &Key,
        single: bool,
        discarded: &std::collections::HashSet<String>,
        listed: &std::collections::HashSet<String>,
    ) {
        // The lock also orders file writes, so a slower concurrent save cannot persist an
        // older map over a newer one.
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        sessions.retain(|known, _| !discarded.contains(known));
        // Entries missing from the listing may belong to a save that finished after it, so
        // they go only once forgotten sessions accumulate.
        if sessions.len() > listed.len() + LINEAGE_STALE {
            sessions.retain(|known, _| listed.contains(known));
        }
        sessions.insert(
            id.to_owned(),
            Lineage {
                key: Arc::new(key.lineage()),
                single,
            },
        );
        if let Some(root) = &self.root {
            let file = LineageFile {
                schema_version: 1,
                sessions: sessions
                    .iter()
                    .map(|(id, saved)| (id.clone(), saved.key.to_record(saved.single)))
                    .collect(),
            };
            let _ = root.write_json(LINEAGE_FILE, &file);
        }
    }
}

pub(super) struct SlotTable {
    entries: Box<[Arc<AsyncMutex<SlotEntry>>]>,
    shape_bound: std::sync::OnceLock<Option<u64>>,
    learned_bytes_per_token: AtomicU64,
    learned_overhead_bytes: AtomicU64,
    /// Tokens and bytes of the largest state this worker has saved.
    largest_save: Mutex<Option<(u64, u64)>>,
    prefill_tokens_per_second: Rate,
    restore_bytes_per_second: Rate,
    sequence: AtomicU64,
}

/// A running average of an observed rate; zero until the first observation.
#[derive(Default)]
struct Rate(AtomicU64);

impl Rate {
    fn observe(&self, rate: f64) {
        if !rate.is_finite() || rate <= 0.0 {
            return;
        }
        let _ = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |bits| {
                let known = f64::from_bits(bits);
                Some(
                    if known > 0.0 {
                        (known + rate) / 2.0
                    } else {
                        rate
                    }
                    .to_bits(),
                )
            });
    }

    fn get(&self) -> Option<f64> {
        Some(f64::from_bits(self.0.load(Ordering::Acquire))).filter(|rate| *rate > 0.0)
    }
}

impl SlotTable {
    pub(super) fn new(slots: u32) -> Self {
        Self {
            entries: (0..slots.max(1))
                .map(|_| Arc::new(AsyncMutex::new(SlotEntry::default())))
                .collect(),
            shape_bound: std::sync::OnceLock::new(),
            learned_bytes_per_token: AtomicU64::new(0),
            learned_overhead_bytes: AtomicU64::new(0),
            largest_save: Mutex::new(None),
            prefill_tokens_per_second: Rate::default(),
            restore_bytes_per_second: Rate::default(),
            sequence: AtomicU64::new(0),
        }
    }

    fn rates(&self) -> crate::sessions::Rates {
        crate::sessions::Rates {
            prefill_tokens_per_second: self.prefill_tokens_per_second.get(),
            restore_bytes_per_second: self.restore_bytes_per_second.get(),
        }
    }

    async fn claim(&self, request: Option<(&Scope, &Key)>) -> (u32, OwnedMutexGuard<SlotEntry>) {
        let mut open = Vec::with_capacity(self.entries.len());
        for (index, entry) in self.entries.iter().enumerate() {
            if let Ok(guard) = entry.clone().try_lock_owned() {
                open.push((index, guard));
            }
        }
        if open.is_empty() {
            return (0, self.entries[0].clone().lock_owned().await);
        }
        let candidates = open
            .iter()
            .map(|(_, entry)| crate::sessions::SlotCandidate {
                holder: entry.holder.as_ref().map(|held| (&held.scope, &held.key)),
                dirty: entry.dirty.is_some(),
                last_used: entry.last_used,
            })
            .collect::<Vec<_>>();
        let best = crate::sessions::slot_preference(&candidates, request)[0];
        let (index, guard) = open.swap_remove(best);
        (index as u32, guard)
    }
}

#[derive(Default)]
pub(super) struct SlotEntry {
    holder: Option<Holder>,
    dirty: Option<u64>,
    last_used: u64,
    generation: u64,
}

#[derive(Clone)]
struct Holder {
    scope: Scope,
    key: Key,
    tokens: Option<u64>,
    /// The request extended an earlier one from the same client, so this is a conversation
    /// and no sibling's state may replace it.
    continued: bool,
    /// The request completed, so the slot holds all of it. Otherwise the engine may hold any
    /// part: the slot stays first choice for a retry, but nothing is saved from it or assumed
    /// about what it holds.
    settled: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct SessionRecord {
    pub path: &'static str,
    pub key: Option<KeyKind>,
    pub slot: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub restore_ms: Option<u64>,
    pub saved_previous: bool,
}

impl SessionRecord {
    /// Corrects the path chosen before the request ran with what the engine reports it reused.
    pub(super) fn observe(&mut self, usage: &crate::engine_usage::Observation) {
        let Some(cached) = usage.cached_tokens else {
            return;
        };
        self.path = match self.path {
            "cold" if cached >= crate::sessions::REUSED_TOKENS_FLOOR => "slot-hit",
            "slot-hit" if cached == 0 => "cold",
            path => path,
        };
    }
}

pub(super) struct Ticket {
    entry: Option<OwnedMutexGuard<SlotEntry>>,
    slot: u32,
    pending: Option<Holder>,
    save: bool,
    pub(super) record: SessionRecord,
}

impl Ticket {
    pub(super) fn settle(
        mut self,
        manager: &Arc<Manager>,
        worker: &Arc<Worker>,
        completed: bool,
        usage: &crate::engine_usage::Observation,
    ) {
        let Some(mut entry) = self.entry.take() else {
            return;
        };
        entry.last_used = unix_time_ms();
        entry.dirty = None;
        entry.holder = self.pending.take();
        if !completed {
            return;
        }
        if let Some(rate) = usage.prompt_tokens_per_second.filter(|_| {
            usage
                .prompt_processed_tokens
                .is_some_and(|tokens| tokens >= RATED_PROMPT_TOKENS)
        }) {
            worker.sessions.prefill_tokens_per_second.observe(rate);
        }
        if let Some(holder) = entry.holder.as_mut() {
            holder.settled = true;
            holder.tokens = usage
                .input_tokens
                .map(|input| input.saturating_add(usage.output_tokens.unwrap_or(0)));
            if self.save {
                let sequence = worker.sessions.sequence.fetch_add(1, Ordering::AcqRel) + 1;
                entry.dirty = Some(sequence);
                manager.schedule_session_save(worker.clone(), self.slot, sequence);
            }
        }
    }
}

struct Found {
    id: String,
    identity: SnapshotIdentity,
    /// Request bytes the saved state shares with the request.
    shared: u64,
    charged: u64,
}

#[derive(Default)]
struct Search {
    /// The saved state worth restoring over what the slot holds.
    restore: Option<Found>,
    /// A saved state is this conversation at an earlier turn, or at this turn after it was
    /// continued before.
    continued: bool,
}

#[derive(Serialize)]
struct SessionRow {
    session: String,
    kind: KeyKind,
    protocol: String,
    model: Option<String>,
    model_tag: String,
    principal: String,
    state: crate::cache::SnapshotState,
    bytes: u64,
    last_used_unix_ms: Option<u64>,
    execution_current: Option<bool>,
    retained: bool,
    expires_unix_ms: Option<u64>,
}

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SessionAction {
    Configure {
        expected_generation: u64,
        settings: Settings,
    },
    Forget {
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        session: Option<String>,
    },
    Retain {
        session: String,
        retained: bool,
    },
}

fn session_budget_error() -> ApiError {
    ApiError::new(
        StatusCode::INSUFFICIENT_STORAGE,
        "session_budget",
        "The session snapshot budget cannot hold this state.",
    )
}

fn claims_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::BAD_GATEWAY,
        "state_transport_uncertain",
        "A session state transfer did not reach completion. Its generation retains physical and file ownership until acknowledged cleanup.",
    )
}

fn other_settings(identity: &SnapshotIdentity, model_tag: &str, fingerprint: &str) -> bool {
    identity.execution_fingerprint != fingerprint
        && crate::sessions::parse_protocol(&identity.protocol)
            .is_some_and(|protocol| protocol.model_tag == model_tag)
}

/// Deletes the saved session states `gone` selects, except kept, busy and unsettled ones, and
/// returns how many went.
fn forget_sessions(
    cache: &CacheStore,
    settings: &Settings,
    gone: impl Fn(&SnapshotIdentity) -> bool,
) -> usize {
    let Ok(rows) = cache.session_rows(
        crate::sessions::PROTOCOL_PREFIX,
        |identity| gone(identity) && !settings.retained.contains(&identity.session_digest),
        false,
    ) else {
        return 0;
    };
    rows.iter()
        .filter(|row| {
            !row.busy
                && row.state == crate::cache::SnapshotState::Saved
                && cache.forget(&row.id, &row.identity, row.revision).is_ok()
        })
        .count()
}

impl Manager {
    pub(super) fn session_settings(&self) -> Arc<Settings> {
        self.session_settings
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .clone()
    }

    pub(super) fn session_mode(&self, worker: &Worker) -> (Mode, Option<&'static str>) {
        let configured = self.session_settings().mode_for(&worker.spec.id);
        if configured == Mode::Off {
            return (Mode::Off, None);
        }
        if worker.spec.execution_profile.adapter != crate::config::Provider::LlamaCpp {
            return (Mode::Off, Some("engine_unsupported"));
        }
        if self.memory_protected() {
            return (Mode::Off, Some("protected_memory"));
        }
        if configured == Mode::Persistent && self.cache.is_none() {
            return (Mode::Memory, Some("cache_storage_unavailable"));
        }
        (configured, None)
    }

    pub(super) fn refuse_session_owned(&self, worker: &Worker) -> Result<(), ApiError> {
        if self.session_mode(worker).0 == Mode::Persistent {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "cache_sessions_managed",
                "Automatic session snapshots own this model's slots. Set its session cache mode to memory or off for manual save and restore.",
            ));
        }
        Ok(())
    }

    pub(super) fn automatic_eligible(worker: &Worker, slot: u32) -> Result<(), ApiError> {
        if worker.spec.execution_profile.adapter != crate::config::Provider::LlamaCpp
            || slot >= worker.spec.max_inflight
        {
            return Err(ApiError::new(
                StatusCode::NOT_IMPLEMENTED,
                "cache_profile_unsupported",
                "Automatic session snapshots need a llama.cpp slot owned by this worker.",
            ));
        }
        Ok(())
    }

    fn session_used(&self, id: &str, fallback: Option<u64>) -> u64 {
        self.session_used
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(id)
            .copied()
            .or(fallback)
            .unwrap_or(0)
    }

    fn touch_session(&self, id: &str) {
        let mut used = self
            .session_used
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if used.len() >= 16384 {
            used.clear();
        }
        used.insert(id.to_owned(), unix_time_ms());
    }

    pub(crate) async fn prepare_session(
        self: &Arc<Self>,
        lease: &mut RequestLease,
        operation: Operation,
        body: &axum::body::Bytes,
        hint: crate::sessions::Hint<'_>,
        context: Option<&RequestContext>,
    ) -> Result<Option<axum::body::Bytes>, ApiError> {
        let worker = lease.worker.clone();
        if !matches!(
            operation,
            Operation::Chat | Operation::Completion | Operation::Responses | Operation::Embeddings
        ) || lease._physical.is_none()
        {
            return Ok(None);
        }
        let (mode, _) = self.session_mode(&worker);
        let multi = worker.sessions.entries.len() > 1;
        if mode == Mode::Off || (mode == Mode::Memory && !multi) {
            return Ok(None);
        }
        let bypass = crate::sessions::prompt_cache_disabled(body);
        let key = context
            .filter(|_| operation != Operation::Embeddings && !bypass)
            .and_then(|_| {
                hint.session
                    .map(|session| Key::header(session, operation, body))
                    .or_else(|| Key::prefix(operation, body))
            });
        let scope = key.as_ref().zip(context).and_then(|(key, context)| {
            Some(Scope {
                principal: context.principal,
                credential_epoch: context.credential_epoch.clone(),
                protocol: crate::sessions::protocol(operation, key.kind, &worker.spec.id)?,
            })
        });
        let request = scope.as_ref().zip(key.as_ref());
        let (slot, mut entry) = worker.sessions.claim(request).await;
        if entry.generation != lease.generation {
            *entry = SlotEntry {
                generation: lease.generation,
                ..SlotEntry::default()
            };
        }
        let persistent = mode == Mode::Persistent;
        let mut record = SessionRecord {
            path: if bypass { "bypass" } else { "cold" },
            key: key.as_ref().map(|key| key.kind),
            slot,
            restore_ms: None,
            saved_previous: false,
        };
        let cell: ClaimCell = Arc::new(Mutex::new(lease._physical.take()));
        if persistent
            && entry.dirty.is_some()
            && let Some(holder) = entry.holder.clone()
            && !request
                .is_some_and(|(scope, key)| holder.scope == *scope && key.extends(&holder.key))
        {
            let generation = entry.generation;
            let manager = self.clone();
            let owner = worker.clone();
            let lent = cell.clone();
            let saved = tokio::spawn(async move {
                manager
                    .session_save(&owner, slot, &holder, generation, Some(lent))
                    .await
            })
            .await;
            record.saved_previous = matches!(saved, Ok(Ok(())));
            entry.dirty = None;
        }
        let held = entry
            .holder
            .as_ref()
            .zip(request)
            .filter(|(held, (scope, _))| held.settled && held.scope == **scope);
        let held_bytes = held.map_or(0, |(held, (_, key))| held.key.shared_bytes(key));
        let extends_held = held.is_some_and(|(held, (_, key))| {
            key.extends(&held.key) && (held.continued || key.units() > held.key.units())
        });
        if held_bytes > 0 {
            record.path = "slot-hit";
        }
        let mut continued = extends_held || key.as_ref().is_some_and(Key::carries_reply);
        if persistent && let Some((scope, key)) = request {
            let found = self
                .find_session(
                    scope,
                    key,
                    lease.fingerprint.clone(),
                    held_bytes,
                    worker.sessions.rates(),
                )
                .await;
            continued |= found.continued;
            if let Some(found) = found.restore {
                entry.holder = None;
                entry.dirty = None;
                record.path = "cold";
                let started = Instant::now();
                let manager = self.clone();
                let owner = worker.clone();
                let lent = cell.clone();
                let state_bytes = found.charged;
                let restored = tokio::spawn(async move {
                    manager
                        .session_restore(&owner, slot, &found, Some(lent))
                        .await
                })
                .await;
                if matches!(restored, Ok(Ok(()))) {
                    let elapsed = started.elapsed();
                    record.path = "restored";
                    record.restore_ms = Some(elapsed.as_millis().min(u64::MAX as u128) as u64);
                    if state_bytes >= RATED_STATE_BYTES {
                        worker
                            .sessions
                            .restore_bytes_per_second
                            .observe(state_bytes as f64 / elapsed.as_secs_f64());
                    }
                }
            }
        }
        lease._physical = cell
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if lease._physical.is_none() {
            return Err(claims_unavailable());
        }
        lease.session = Some(Ticket {
            entry: Some(entry),
            slot,
            pending: scope.zip(key).map(|(scope, key)| Holder {
                scope,
                key,
                tokens: None,
                continued,
                settled: false,
            }),
            save: persistent && !hint.no_store,
            record,
        });
        Ok((multi && operation != Operation::Embeddings)
            .then(|| crate::sessions::with_slot(body, slot))
            .flatten()
            .map(axum::body::Bytes::from))
    }

    /// Looks for a saved state that holds more of the request than the `held` bytes its slot
    /// already does. A state of this conversation is always worth restoring; one that only
    /// opens alike must repay its restore.
    async fn find_session(
        &self,
        scope: &Scope,
        key: &Key,
        fingerprint: Arc<str>,
        held: u64,
        rates: crate::sessions::Rates,
    ) -> Search {
        let Some(cache) = self.cache.clone() else {
            return Search::default();
        };
        let scope = scope.clone();
        let settings = self.session_settings();
        let turns = key.units();
        let candidates = key
            .candidates()
            .map(|(units, digest)| (digest, (units, key.unit_bytes(units))))
            .collect::<std::collections::HashMap<_, _>>();
        let shared = self.session_lineage.shared_with(key);
        let lineage = self.session_lineage.clone();
        tokio::task::spawn_blocking(move || {
            let mut search = Search::default();
            let Ok(rows) = cache.session_rows(
                &scope.protocol,
                |identity| {
                    identity.principal == scope.principal
                        && identity.credential_epoch == *scope.credential_epoch
                        && identity.protocol == scope.protocol
                },
                false,
            ) else {
                return search;
            };
            let mut latest_turn = 0;
            for row in rows {
                let exact = candidates.get(&row.identity.session_digest).copied();
                let Some(bytes) = exact
                    .map(|(_, bytes)| bytes)
                    .max(shared.get(&row.id).copied())
                else {
                    continue;
                };
                if row.state != crate::cache::SnapshotState::Saved {
                    continue;
                }
                if row.identity.execution_fingerprint != *fingerprint {
                    if !row.busy && !settings.retained.contains(&row.identity.session_digest) {
                        let _ = cache.forget(&row.id, &row.identity, row.revision);
                    }
                    continue;
                }
                if let Some((units, _)) = exact
                    && units > latest_turn
                {
                    latest_turn = units;
                    search.continued = units < turns || !lineage.single(&row.id);
                }
                let pays = bytes > held
                    && (exact.is_some()
                        || crate::sessions::worth_restoring(bytes - held, row.charged, rates));
                // Equal shared bytes diverge at the same place, so the smaller state restores
                // the same reusable opening faster.
                if pays
                    && search.restore.as_ref().is_none_or(|best| {
                        (bytes, std::cmp::Reverse(row.charged))
                            > (best.shared, std::cmp::Reverse(best.charged))
                    })
                {
                    search.restore = Some(Found {
                        id: row.id,
                        identity: row.identity,
                        shared: bytes,
                        charged: row.charged,
                    });
                }
            }
            search
        })
        .await
        .unwrap_or_default()
    }

    async fn session_restore(
        &self,
        worker: &Arc<Worker>,
        slot: u32,
        found: &Found,
        lender: Option<ClaimCell>,
    ) -> Result<(), ApiError> {
        let cache = self.cache_store()?;
        let budget = self.state_budget(worker)?;
        let activity = self
            .cache_activity(false, crate::cache::memory::Work::Bounded)
            .await?;
        let (owner, _cancellation) = self
            .state_operation(
                worker.clone(),
                slot,
                "",
                StateAction::Restore,
                budget,
                activity,
                lender,
                true,
            )
            .await?;
        let result = self
            .restore_slot(
                owner,
                cache.clone(),
                found.id.clone(),
                found.identity.clone(),
                slot,
                true,
            )
            .await;
        match result {
            Ok(_) => {
                self.touch_session(&found.id);
                Ok(())
            }
            Err(error) => {
                if error.code == "snapshot_corrupt" || error.code == "cache_restore_rejected" {
                    let id = found.id.clone();
                    let identity = found.identity.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        if let Ok(rows) =
                            cache.session_rows(&identity.protocol, |row| *row == identity, false)
                            && let Some(row) =
                                rows.into_iter().find(|row| row.id == id && !row.busy)
                        {
                            let _ = cache.forget(&row.id, &row.identity, row.revision);
                        }
                    })
                    .await;
                }
                Err(error)
            }
        }
    }

    fn shape_bound(worker: &Worker) -> Option<u64> {
        *worker.sessions.shape_bound.get_or_init(|| {
            let mut file = std::fs::File::open(&worker.spec.model_file).ok()?;
            let (_, shape) = crate::profiles::read_gguf_shape(&mut file).ok()?;
            crate::sessions::per_token_bound(&shape)
        })
    }

    async fn session_save(
        &self,
        worker: &Arc<Worker>,
        slot: u32,
        holder: &Holder,
        generation: u64,
        lender: Option<ClaimCell>,
    ) -> Result<(), ApiError> {
        let _permit = self
            .session_saves
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| claims_unavailable())?;
        let cache = self.cache_store()?;
        let settings = self.session_settings();
        let budget = self.state_budget(worker)?;
        let activity = self
            .cache_activity(false, crate::cache::memory::Work::Bounded)
            .await?;
        let (owner, _cancellation) = self
            .state_operation(
                worker.clone(),
                slot,
                "",
                StateAction::Save,
                budget,
                activity,
                lender,
                true,
            )
            .await?;
        if owner.generation() != generation {
            return Err(ApiError::new(
                StatusCode::CONFLICT,
                "worker_changed",
                "The slot belongs to an earlier worker generation.",
            ));
        }
        let identity = SnapshotIdentity {
            execution_fingerprint: owner.fingerprint().to_string(),
            principal: holder.scope.principal.to_owned(),
            credential_epoch: holder.scope.credential_epoch.to_string(),
            protocol: holder.scope.protocol.clone(),
            session_digest: holder.key.saved_digest(),
        };
        let shape = tokio::task::spawn_blocking({
            let worker = worker.clone();
            move || Self::shape_bound(&worker)
        })
        .await
        .ok()
        .flatten();
        let learned = Some(
            worker
                .sessions
                .learned_bytes_per_token
                .load(Ordering::Acquire),
        )
        .filter(|value| *value > 0);
        let context = worker
            .spec
            .execution_profile
            .resolved_arguments()
            .ok()
            .and_then(|(_, options)| options?.context_tokens)
            .map(u64::from);
        let known = shape.is_some() || learned.is_some();
        let observed = worker
            .sessions
            .largest_save
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .map(|(tokens, bytes)| bytes.div_ceil(tokens));
        let bound = crate::sessions::reservation(
            holder.tokens,
            context,
            shape,
            learned,
            observed,
            worker
                .sessions
                .learned_overhead_bytes
                .load(Ordering::Acquire),
            settings.model_budget_bytes,
        );
        let model_tag = crate::sessions::model_tag(&worker.spec.id);
        let superseded = holder
            .key
            .ancestors()
            .chain(std::iter::once(holder.key.saved_digest()))
            .collect::<std::collections::HashSet<_>>();
        let budgets = crate::sessions::Budgets {
            model_bytes: settings.model_budget_bytes,
            total_bytes: settings.budget_bytes,
            sessions: settings.max_sessions as usize,
        };
        self.reclaim_sessions().await;
        self.expire_sessions().await;
        let rows = self.all_session_rows(&cache).await?;
        let stored = self.stored_rows(&rows, &identity, &model_tag, &superseded, None);
        let estimate = if known {
            bound
        } else {
            stored
                .iter()
                .filter(|row| row.model_tag == model_tag)
                .map(|row| row.charged)
                .max()
                .unwrap_or(0)
        };
        let plan = crate::sessions::plan_eviction(&stored, &model_tag, estimate, &budgets)
            .ok_or_else(session_budget_error)?;
        let evicted: u64 = plan.iter().map(|index| stored[*index].charged).sum();
        self.require_space(
            vec![(self.cache_directory(), estimate.saturating_sub(evicted))],
            "save this session",
        )
        .await?;
        let evictions = plan
            .into_iter()
            .map(|index| {
                let row = &rows[index];
                (row.id.clone(), row.identity.clone(), row.revision)
            })
            .collect();
        let (owner, metadata, acknowledgement) = self
            .save_slot(
                owner,
                cache.clone(),
                identity.clone(),
                slot,
                None,
                bound,
                settings.model_budget_bytes,
                evictions,
            )
            .await?;
        drop(owner);
        if let (Some(tokens), Some(bytes)) = (
            acknowledgement["n_saved"].as_u64(),
            acknowledgement["n_written"].as_u64(),
        ) && tokens > 0
        {
            let mut largest = worker
                .sessions
                .largest_save
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if largest.is_none_or(|(most, _)| tokens >= most) {
                *largest = Some((tokens, bytes));
            }
            drop(largest);
            // A known KV shape already bounds per-token growth; what it misses is fixed state
            // such as recurrent layers and context checkpoints, which must not scale with tokens.
            match shape {
                Some(per_token) => worker.sessions.learned_overhead_bytes.fetch_max(
                    bytes.saturating_sub(tokens.saturating_mul(per_token)),
                    Ordering::AcqRel,
                ),
                None => worker
                    .sessions
                    .learned_bytes_per_token
                    .fetch_max(bytes.div_ceil(tokens), Ordering::AcqRel),
            };
        }
        self.touch_session(&metadata.id);
        let saved = metadata.id;
        let rows = self.all_session_rows(&cache).await?;
        let mut stored = self.stored_rows(&rows, &identity, &model_tag, &superseded, Some(&saved));
        let same_client = |row: &crate::cache::SessionRow| {
            row.id != saved
                && row.identity.principal == identity.principal
                && row.identity.credential_epoch == identity.credential_epoch
                && row.identity.protocol == identity.protocol
        };
        let earlier_turn = |row: &crate::cache::SessionRow| {
            same_client(row) && superseded.contains(&row.identity.session_digest)
        };
        let single = !holder.continued
            && !rows.iter().any(|row| {
                earlier_turn(row) && row.identity.session_digest != identity.session_digest
            });
        let siblings = self.session_lineage.siblings(&holder.key);
        let saved_at = (self.session_used(&saved, None), saved.as_str());
        // A sibling saved after this one is left for its own save to judge, so two saves that
        // finish together cannot delete each other.
        let replaced_sibling = |row: &crate::cache::SessionRow| {
            same_client(row)
                && siblings.contains(&row.id)
                && row.identity.execution_fingerprint == identity.execution_fingerprint
                && row.state == crate::cache::SnapshotState::Saved
                && !row.busy
                && (self.session_used(&row.id, row.modified_ms), row.id.as_str()) < saved_at
        };
        let mut discard = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                // A kept state stays as it was pinned; only a repeat of the same request,
                // which carries the pin, replaces it.
                (earlier_turn(row) || replaced_sibling(row))
                    && (row.identity.session_digest == identity.session_digest
                        || !settings.retained.contains(&row.identity.session_digest))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in &discard {
            stored[*index].busy = true;
            stored[*index].charged = 0;
        }
        discard.extend(
            crate::sessions::plan_eviction(
                &stored,
                &model_tag,
                0,
                &crate::sessions::Budgets {
                    sessions: budgets.sessions.saturating_add(1 + discard.len()),
                    ..budgets
                },
            )
            .unwrap_or_default(),
        );
        let listed = rows
            .iter()
            .map(|row| row.id.clone())
            .collect::<std::collections::HashSet<_>>();
        let discarded = discard
            .iter()
            .map(|index| rows[*index].id.clone())
            .collect::<std::collections::HashSet<_>>();
        let targets = discard
            .into_iter()
            .map(|index| {
                let row = &rows[index];
                (row.id.clone(), row.identity.clone(), row.revision)
            })
            .collect::<Vec<_>>();
        let lineage = self.session_lineage.clone();
        let key = holder.key.clone();
        let _ = tokio::task::spawn_blocking(move || {
            for (id, identity, revision) in targets {
                let _ = cache.forget(&id, &identity, revision);
            }
            lineage.record(&saved, &key, single, &discarded, &listed);
        })
        .await;
        Ok(())
    }

    // An interrupted automatic save stays charged against the session budget until its writer
    // exits. Once the cache can prove that, the space is returned without operator action.
    pub(super) async fn reclaim_sessions(&self) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        let _ = tokio::task::spawn_blocking(move || {
            let Ok(rows) = cache.session_rows(crate::sessions::PROTOCOL_PREFIX, |_| true, false)
            else {
                return;
            };
            let control = crate::cache::OperationControl::new(
                Instant::now() + Duration::from_secs(2),
                Arc::new(AtomicBool::new(false)),
            );
            for row in rows {
                if row.state != crate::cache::SnapshotState::Saved && !row.busy {
                    let _ = cache.recover_writer(&row.id, &row.identity, row.revision, &control);
                }
            }
        })
        .await;
    }

    async fn all_session_rows(
        &self,
        cache: &Arc<CacheStore>,
    ) -> Result<Vec<crate::cache::SessionRow>, ApiError> {
        let cache = cache.clone();
        tokio::task::spawn_blocking(move || {
            cache.session_rows(crate::sessions::PROTOCOL_PREFIX, |_| true, true)
        })
        .await
        .map_err(|_| claims_unavailable())?
        .map_err(cache_error)
    }

    fn stored_rows(
        &self,
        rows: &[crate::cache::SessionRow],
        identity: &SnapshotIdentity,
        model_tag: &str,
        superseded: &std::collections::HashSet<String>,
        saved: Option<&str>,
    ) -> Vec<crate::sessions::StoredRow> {
        let retained = &self.session_settings().retained;
        rows.iter()
            .map(|row| {
                let tag = crate::sessions::parse_protocol(&row.identity.protocol)
                    .map(|tag| tag.model_tag.to_owned())
                    .unwrap_or_default();
                let stale = tag == model_tag
                    && row.identity.execution_fingerprint != identity.execution_fingerprint;
                let replaced = row.identity.principal == identity.principal
                    && row.identity.credential_epoch == identity.credential_epoch
                    && row.identity.protocol == identity.protocol
                    && superseded.contains(&row.identity.session_digest);
                crate::sessions::StoredRow {
                    id: row.id.clone(),
                    charged: row.charged,
                    model_tag: tag,
                    last_used: if stale || replaced {
                        0
                    } else {
                        self.session_used(&row.id, row.modified_ms)
                    },
                    busy: row.busy
                        || row.state != crate::cache::SnapshotState::Saved
                        || saved == Some(row.id.as_str())
                        || retained.contains(&row.identity.session_digest),
                }
            })
            .collect()
    }

    pub(super) fn schedule_session_save(
        self: &Arc<Self>,
        worker: Arc<Worker>,
        slot: u32,
        sequence: u64,
    ) {
        let delay = Duration::from_millis(self.session_settings().save_delay_ms);
        let manager = Arc::downgrade(self);
        tokio::spawn(async move {
            for attempt in 1..=SAVE_ATTEMPTS {
                tokio::time::sleep(
                    delay
                        .saturating_mul(attempt)
                        .max(Duration::from_millis(10 * u64::from(attempt))),
                )
                .await;
                let Some(manager) = manager.upgrade() else {
                    return;
                };
                if manager.shutting_down.load(Ordering::Acquire) {
                    return;
                }
                let Ok(mut entry) = worker.sessions.entries[slot as usize]
                    .clone()
                    .try_lock_owned()
                else {
                    return;
                };
                if entry.dirty != Some(sequence) {
                    return;
                }
                let Some(holder) = entry.holder.clone() else {
                    return;
                };
                match manager
                    .session_save(&worker, slot, &holder, entry.generation, None)
                    .await
                {
                    Err(error) if error.code == "worker_busy" && attempt < SAVE_ATTEMPTS => {}
                    Err(error) => {
                        manager.logs.service(
                            crate::logs::Level::Warning,
                            &format!(
                                "Session snapshot for {} slot {slot} was not saved: {}",
                                worker.spec.id, error.code
                            ),
                        );
                        entry.dirty = None;
                        return;
                    }
                    Ok(()) => {
                        entry.dirty = None;
                        return;
                    }
                }
            }
        });
    }

    pub(super) async fn flush_sessions(&self, worker: &Arc<Worker>) {
        if self.session_mode(worker).0 != Mode::Persistent {
            return;
        }
        for (slot, entry) in worker.sessions.entries.iter().enumerate() {
            let Ok(mut entry) = entry.clone().try_lock_owned() else {
                continue;
            };
            if entry.dirty.is_none() {
                continue;
            }
            if let Some(holder) = entry.holder.clone() {
                let _ = self
                    .session_save(worker, slot as u32, &holder, entry.generation, None)
                    .await;
            }
            entry.dirty = None;
        }
    }

    pub async fn sessions_view(&self) -> Result<serde_json::Value, ApiError> {
        let settings = self.session_settings();
        let configuration_generation = self.config.lock().await.generation;
        let explicit = self.config.lock().await.cache_enabled;
        let workers = self.worker_snapshot();
        let tags = workers
            .iter()
            .map(|worker| {
                (
                    crate::sessions::model_tag(&worker.spec.id),
                    worker.spec.id.clone(),
                )
            })
            .collect::<std::collections::HashMap<_, _>>();
        let current = workers
            .iter()
            .filter_map(|worker| {
                let state = worker.state.try_lock().ok()?;
                let run = state.running.as_ref()?;
                Some((
                    crate::sessions::model_tag(&worker.spec.id),
                    run.fingerprint.clone(),
                ))
            })
            .collect::<std::collections::HashMap<_, _>>();
        let rows = match self.cache.clone() {
            Some(cache) => tokio::task::spawn_blocking(move || {
                cache.session_rows(crate::sessions::PROTOCOL_PREFIX, |_| true, true)
            })
            .await
            .map_err(|_| claims_unavailable())?
            .map_err(cache_error)?,
            None => Vec::new(),
        };
        let mut sessions = rows
            .iter()
            .filter_map(|row| {
                let tag = crate::sessions::parse_protocol(&row.identity.protocol)?;
                Some(SessionRow {
                    session: row.identity.session_digest.clone(),
                    kind: tag.kind,
                    protocol: tag.operation.to_owned(),
                    model: tags.get(tag.model_tag).cloned(),
                    model_tag: tag.model_tag.to_owned(),
                    principal: row.identity.principal.clone(),
                    state: row.state,
                    bytes: row.charged,
                    last_used_unix_ms: Some(self.session_used(&row.id, row.modified_ms))
                        .filter(|value| *value > 0),
                    execution_current: current
                        .get(tag.model_tag)
                        .map(|fingerprint| **fingerprint == *row.identity.execution_fingerprint),
                    retained: settings.retained.contains(&row.identity.session_digest),
                    expires_unix_ms: settings
                        .retention_days
                        .filter(|_| !settings.retained.contains(&row.identity.session_digest))
                        .map(|days| {
                            self.session_used(&row.id, row.modified_ms)
                                .saturating_add(u64::from(days) * 86_400_000)
                        }),
                })
            })
            .collect::<Vec<_>>();
        sessions.sort_by_key(|row| std::cmp::Reverse(row.last_used_unix_ms));
        let total_bytes = sessions
            .iter()
            .fold(0u64, |total, row| total.saturating_add(row.bytes));
        let total_sessions = sessions.len();
        let models = workers
            .iter()
            .map(|worker| {
                let tag = crate::sessions::model_tag(&worker.spec.id);
                let (effective, reason) = self.session_mode(worker);
                let (used, count) = sessions
                    .iter()
                    .filter(|row| row.model_tag == tag)
                    .fold((0u64, 0usize), |(bytes, count), row| {
                        (bytes.saturating_add(row.bytes), count + 1)
                    });
                let slots = worker
                    .sessions
                    .entries
                    .iter()
                    .enumerate()
                    .map(|(slot, entry)| match entry.try_lock() {
                        Ok(entry) => serde_json::json!({"slot":slot,"state":if entry.dirty.is_some() {"unsaved"} else if entry.holder.is_some() {"holding"} else {"empty"},"units":entry.holder.as_ref().map(|held| held.key.units()),"tokens":entry.holder.as_ref().and_then(|held| held.tokens)}),
                        Err(_) => serde_json::json!({"slot":slot,"state":"busy","units":null,"tokens":null}),
                    })
                    .collect::<Vec<_>>();
                serde_json::json!({"model":worker.spec.id,"mode":settings.mode_for(&worker.spec.id),"effective":effective,"reason":reason,"budget_bytes":settings.model_budget_bytes,"used_bytes":used,"sessions":count,"slots":slots})
            })
            .collect::<Vec<_>>();
        sessions.truncate(LISTED_SESSIONS);
        Ok(
            serde_json::json!({"schema_version":1,"configuration_generation":configuration_generation,"settings":&*settings,"explicit_snapshots_enabled":explicit,"models":models,"totals":{"sessions":total_sessions,"bytes":total_bytes,"budget_bytes":settings.budget_bytes,"listed":sessions.len()},"sessions":sessions,"contains_content":false}),
        )
    }

    pub(super) async fn forget_session_rows(
        &self,
        tag: Option<String>,
        session: Option<String>,
    ) -> Result<(usize, usize), ApiError> {
        let cache = self.cache_store()?;
        let (forgotten, busy) = tokio::task::spawn_blocking(move || {
            let rows = cache.session_rows(
                crate::sessions::PROTOCOL_PREFIX,
                |identity| {
                    crate::sessions::parse_protocol(&identity.protocol).is_some_and(|protocol| {
                        tag.as_deref().is_none_or(|tag| protocol.model_tag == tag)
                    }) && session
                        .as_deref()
                        .is_none_or(|session| identity.session_digest == session)
                },
                false,
            )?;
            let mut forgotten = 0usize;
            let mut busy = 0usize;
            for row in rows {
                if !row.busy && row.state == crate::cache::SnapshotState::Uncertain {
                    let control = crate::cache::OperationControl::new(
                        Instant::now() + Duration::from_secs(2),
                        Arc::new(AtomicBool::new(false)),
                    );
                    match cache.recover_writer(&row.id, &row.identity, row.revision, &control) {
                        Ok(outcome) if outcome.recovered => forgotten += 1,
                        _ => busy += 1,
                    }
                    continue;
                }
                if row.busy || row.state != crate::cache::SnapshotState::Saved {
                    busy += 1;
                    continue;
                }
                match cache.forget(&row.id, &row.identity, row.revision) {
                    Ok(()) => forgotten += 1,
                    Err(_) => busy += 1,
                }
            }
            Ok::<_, CacheError>((forgotten, busy))
        })
        .await
        .map_err(|_| claims_unavailable())?
        .map_err(cache_error)?;
        Ok((forgotten, busy))
    }

    async fn commit_session_settings(
        self: &Arc<Self>,
        config: OwnedMutexGuard<crate::config::Config>,
        settings: Settings,
    ) -> Result<u64, ApiError> {
        settings.validate().map_err(|message| {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_session_settings", message)
        })?;
        let mut next = config.clone();
        next.session_cache = settings.clone();
        next.generation = next.generation.checked_add(1).ok_or_else(|| {
            ApiError::new(
                StatusCode::CONFLICT,
                "revision_exhausted",
                "Configuration generation cannot advance.",
            )
        })?;
        let generation = next.generation;
        let outcome = self
            .publish_configuration(config, next, publication::Effect::ConfigOnly)
            .await?;
        *self
            .session_settings
            .write()
            .unwrap_or_else(|error| error.into_inner()) = Arc::new(settings);
        if outcome.warning {
            self.verify_storage().await?;
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "configuration_durability",
                "Session cache settings committed but directory durability is unconfirmed.",
            ));
        }
        Ok(generation)
    }

    /// Deletes a model's saved states that the worker now running cannot restore because they
    /// were made under other engine settings. Kept, busy and unsettled states stay.
    pub(super) async fn drop_stale_sessions(&self, model: &str, fingerprint: &Arc<str>) -> usize {
        let Some(cache) = self.cache.clone() else {
            return 0;
        };
        let settings = self.session_settings();
        let tag = crate::sessions::model_tag(model);
        let current = fingerprint.clone();
        let dropped = tokio::task::spawn_blocking(move || {
            forget_sessions(&cache, &settings, |identity| {
                other_settings(identity, &tag, &current)
            })
        })
        .await
        .unwrap_or(0);
        if dropped > 0 {
            self.logs.service(
                crate::logs::Level::Info,
                &format!(
                    "Deleted {dropped} saved conversation states of {model}: its engine settings changed, so they cannot be restored."
                ),
            );
        }
        dropped
    }

    /// The service's own requests are never saved, so a state under its principal is left from
    /// an earlier build and nothing can continue it.
    pub(super) async fn drop_service_sessions(&self) {
        let Some(cache) = self.cache.clone() else {
            return;
        };
        let settings = self.session_settings();
        let dropped = tokio::task::spawn_blocking(move || {
            forget_sessions(&cache, &settings, |identity| {
                identity.principal == crate::auth::INTERNAL_PRINCIPAL
            })
        })
        .await
        .unwrap_or(0);
        if dropped > 0 {
            self.logs.service(
                crate::logs::Level::Info,
                &format!(
                    "Deleted {dropped} saved states of Juiced's own model checks and Tune trials, which nothing continues."
                ),
            );
        }
    }

    /// Runs the sweep that a worker's start skips while Tune is trying settings.
    pub async fn drop_stale_sessions_after_tune(&self, model: &str) {
        let Ok(worker) = self.worker(model) else {
            return;
        };
        let state = worker.state.lock().await;
        if let Some(run) = &state.running {
            self.drop_stale_sessions(model, &run.fingerprint).await;
        }
    }

    /// Deletes saved sessions unused for longer than the retention period, except retained,
    /// busy and unsettled ones.
    pub(super) async fn expire_sessions(&self) -> usize {
        let settings = self.session_settings();
        let (Some(days), Some(cache)) = (settings.retention_days, self.cache.clone()) else {
            return 0;
        };
        let cutoff = unix_time_ms().saturating_sub(u64::from(days) * 86_400_000);
        let Ok(Ok(rows)) = tokio::task::spawn_blocking({
            let cache = cache.clone();
            move || cache.session_rows(crate::sessions::PROTOCOL_PREFIX, |_| true, false)
        })
        .await
        else {
            return 0;
        };
        let expired = rows
            .into_iter()
            .filter(|row| {
                !row.busy
                    && row.state == crate::cache::SnapshotState::Saved
                    && !settings.retained.contains(&row.identity.session_digest)
                    && (1..cutoff).contains(&self.session_used(&row.id, row.modified_ms))
            })
            .collect::<Vec<_>>();
        tokio::task::spawn_blocking(move || {
            expired
                .iter()
                .filter(|row| cache.forget(&row.id, &row.identity, row.revision).is_ok())
                .count()
        })
        .await
        .unwrap_or(0)
    }

    pub async fn session_action(
        self: &Arc<Self>,
        action: SessionAction,
    ) -> Result<serde_json::Value, ApiError> {
        match action {
            SessionAction::Configure {
                expected_generation,
                mut settings,
            } => {
                let config = self.config.clone().lock_owned().await;
                if config.generation != expected_generation {
                    return Err(ApiError::new(
                        StatusCode::CONFLICT,
                        "revision_conflict",
                        "Configuration changed. Refresh before changing session cache settings.",
                    ));
                }
                settings.retained = config.session_cache.retained.clone();
                let generation = self.commit_session_settings(config, settings).await?;
                self.expire_sessions().await;
                Ok(serde_json::json!({"configured":true,"configuration_generation":generation}))
            }
            SessionAction::Retain { session, retained } => {
                if !crate::sessions::valid_session_digest(&session) {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_session",
                        "Provide a listed 64-character session digest.",
                    ));
                }
                if retained {
                    let cache = self.cache_store()?;
                    let wanted = session.clone();
                    let listed = tokio::task::spawn_blocking(move || {
                        cache.session_rows(
                            crate::sessions::PROTOCOL_PREFIX,
                            |identity| identity.session_digest == wanted,
                            false,
                        )
                    })
                    .await
                    .map_err(|_| claims_unavailable())?
                    .map_err(cache_error)?;
                    if listed.is_empty() {
                        return Err(ApiError::new(
                            StatusCode::NOT_FOUND,
                            "unknown_session",
                            "No saved session has this digest.",
                        ));
                    }
                }
                let config = self.config.clone().lock_owned().await;
                let mut settings = config.session_cache.clone();
                let changed = if retained {
                    settings.retained.insert(session.clone())
                } else {
                    settings.retained.remove(&session)
                };
                let generation = if changed {
                    self.commit_session_settings(config, settings).await?
                } else {
                    config.generation
                };
                Ok(
                    serde_json::json!({"session":session,"retained":retained,"configuration_generation":generation}),
                )
            }
            SessionAction::Forget { model, session } => {
                if session.as_ref().is_some_and(|session| {
                    session.len() != 64 || !session.bytes().all(|byte| byte.is_ascii_hexdigit())
                }) {
                    return Err(ApiError::new(
                        StatusCode::BAD_REQUEST,
                        "invalid_session",
                        "Provide a listed 64-character session digest.",
                    ));
                }
                if let Some(model) = &model {
                    self.worker(model)?;
                }
                let (forgotten, busy) = self
                    .forget_session_rows(model.as_deref().map(crate::sessions::model_tag), session)
                    .await?;
                Ok(serde_json::json!({"forgotten":forgotten,"retained_busy":busy}))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(
        store: &Arc<CacheStore>,
        principal: &str,
        model: &str,
        fingerprint: char,
        session: char,
    ) -> String {
        let identity = SnapshotIdentity {
            execution_fingerprint: fingerprint.to_string().repeat(64),
            principal: principal.into(),
            credential_epoch: "e".repeat(64),
            protocol: crate::sessions::protocol(Operation::Chat, KeyKind::Prefix, model).unwrap(),
            session_digest: session.to_string().repeat(64),
        };
        let pending = store.begin(identity, store.generation(), 1024).unwrap();
        std::fs::write(pending.path(), b"state").unwrap();
        store.publish(&pending, pending.revision()).unwrap();
        session.to_string().repeat(64)
    }

    fn left(store: &CacheStore) -> Vec<String> {
        let mut sessions = store
            .session_rows(crate::sessions::PROTOCOL_PREFIX, |_| true, false)
            .unwrap()
            .into_iter()
            .map(|row| row.identity.session_digest[..1].to_owned())
            .collect::<Vec<_>>();
        sessions.sort();
        sessions
    }

    #[test]
    fn a_record_names_the_path_the_engine_reports_not_the_one_expected() {
        let settled = |path: &'static str, cached: Option<u64>| {
            let mut record = SessionRecord {
                path,
                key: Some(KeyKind::Prefix),
                slot: 0,
                restore_ms: None,
                saved_previous: false,
            };
            record.observe(&crate::engine_usage::Observation {
                cached_tokens: cached,
                ..Default::default()
            });
            record.path
        };
        let floor = crate::sessions::REUSED_TOKENS_FLOOR;
        assert_eq!(settled("cold", Some(41_223)), "slot-hit");
        assert_eq!(settled("cold", Some(floor)), "slot-hit");
        assert_eq!(settled("cold", Some(floor - 1)), "cold");
        assert_eq!(settled("cold", None), "cold");
        assert_eq!(settled("slot-hit", Some(0)), "cold");
        assert_eq!(settled("slot-hit", Some(3)), "slot-hit");
        assert_eq!(settled("slot-hit", None), "slot-hit");
        assert_eq!(settled("restored", Some(0)), "restored");
        assert_eq!(settled("bypass", Some(500)), "bypass");

        let rate = Rate::default();
        assert_eq!(rate.get(), None);
        for ignored in [0.0, -4.0, f64::NAN, f64::INFINITY] {
            rate.observe(ignored);
        }
        assert_eq!(rate.get(), None);
        rate.observe(200.0);
        rate.observe(400.0);
        assert_eq!(rate.get(), Some(300.0));
    }

    #[test]
    fn cleanup_deletes_only_unrestorable_or_service_owned_states_and_never_kept_ones() {
        let directory = tempfile::tempdir().unwrap();
        let lease =
            crate::storage::DirectoryLease::acquire(directory.path().join("cache")).unwrap();
        let store = CacheStore::open(lease).unwrap();
        saved(&store, "configured-api", "chat", 'a', '1');
        saved(&store, "configured-api", "chat", 'b', '2');
        let kept = saved(&store, "configured-api", "chat", 'b', '3');
        saved(&store, "configured-api", "other", 'b', '4');
        saved(&store, crate::auth::INTERNAL_PRINCIPAL, "chat", 'a', '5');
        let kept_check = saved(&store, crate::auth::INTERNAL_PRINCIPAL, "other", 'b', '6');
        let settings = Settings {
            retained: [kept, kept_check].into(),
            ..Settings::default()
        };
        let tag = crate::sessions::model_tag("chat");

        let service = forget_sessions(&store, &settings, |identity| {
            identity.principal == crate::auth::INTERNAL_PRINCIPAL
        });
        assert_eq!(service, 1);
        assert_eq!(left(&store), ["1", "2", "3", "4", "6"]);

        let current = "a".repeat(64);
        let stale = forget_sessions(&store, &settings, |identity| {
            other_settings(identity, &tag, &current)
        });
        assert_eq!(stale, 1);
        assert_eq!(left(&store), ["1", "3", "4", "6"]);
        assert_eq!(
            forget_sessions(&store, &settings, |identity| {
                other_settings(identity, &tag, &current)
            }),
            0
        );
    }
}
