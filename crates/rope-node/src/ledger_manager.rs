//! # Ledger Manager — Node-Level Personal Ledger Integration
//!
//! Wires together all subsystems to provide the complete personal ledger
//! lifecycle within a running Datachain Rope node:
//!
//! - **Creation**: `create_ledger(wallet)` → genesis string + OES key binding
//! - **Append**: `append_to_ledger(wallet, interaction)` → encrypt + slice + distribute
//! - **Repatriate**: `repatriate_ledger(wallet)` → discover + fetch + assemble
//! - **Delete**: `erase_ledger(wallet)` → destroy OES key + tombstone + broadcast
//!
//! ## RPC Methods
//!
//! | Method | Parameters | Returns |
//! |--------|-----------|---------|
//! | `rope_createLedger` | `{ wallet }` | `{ genesis_string_id, status }` |
//! | `rope_appendToLedger` | `{ wallet, interaction }` | `{ string_id, piece_count }` |
//! | `rope_repatriateLedger` | `{ wallet, known_head? }` | `{ entries[], total_bytes }` |
//! | `rope_eraseLedger` | `{ wallet, reason }` | `{ audit_hash, entries_erased }` |
//! | `rope_getLedgerStatus` | `{ wallet }` | `{ descriptor }` |

use crate::lattice_metrics::{instrument_head_lock, LatticeOp};
use crate::oes_key_cache::OESKeyCache;
use rope_core::clock::ClockManager;
use rope_core::lattice::StringLattice;
use rope_core::personal_ledger::{
    build_append_string, build_genesis_string, InteractionRecord, StringKind, StringRegistry,
};
use rope_core::string::PublicKey;
use rope_core::types::{NodeId, StringId};
use rope_crypto::ledger_encryption::{
    decrypt_ledger_content, encrypt_ledger_content, LedgerEnvelope, WalletAddress,
};
use rope_crypto::oes::OESManager;
use rope_protocols::ledger_lifecycle::{
    slice_encrypted_content, DeletionReason, LedgerErasureAudit, LedgerLifecycleManager,
    LifecycleConfig,
};
use rope_storage::LedgerStore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;

/// Durability policy for high-volume ledger writes (`create` / `append`).
///
/// Default **ack-after-enqueue** (2026-07-27 residual-5xx P1): the RPC
/// returns as soon as the write is queued to the RocksDB flusher. The
/// flusher fsyncs within `FLUSH_INTERVAL` (~10 ms). This removes the
/// per-RPC `Condvar::wait_timeout` that — even behind `spawn_blocking` —
/// saturated the blocking pool under DCSwap Quipu bursts and correlated
/// with the ~14 min hang / watchdog SIGKILL cycle after MemoryHigh
/// headroom was already raised.
///
/// Set `ROPE_SYNC_DURABILITY=1` to restore the Phase 1.6 "RPC success ⇒
/// fsync'd" contract (bounded 5 s wait; timeout becomes a hard error).
/// GDPR paths (`erase` / `untie`) always use sync durability regardless.
fn parse_sync_durability(val: Option<&str>) -> bool {
    match val {
        Some(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        None => false,
    }
}

fn sync_durability_enabled() -> bool {
    parse_sync_durability(std::env::var("ROPE_SYNC_DURABILITY").ok().as_deref())
}

/// Map persistence errors to RPC-facing strings. QueueFull becomes an
/// explicit OVERLOAD token so `rpc_server` can emit JSON-RPC `-32005`
/// (retryable; message carries `Retry-After: 1`).
fn persist_err(e: rope_storage::RocksError) -> String {
    match e {
        rope_storage::RocksError::QueueFull => {
            "OVERLOAD: ledger write queue full; Retry-After: 1".to_string()
        }
        other => other.to_string(),
    }
}

/// Optional sync wait for create/append. No-op when ack-after-enqueue.
fn await_durable_create_append(store: &LedgerStore, op: &str) -> Result<(), String> {
    if !sync_durability_enabled() {
        return Ok(());
    }
    if store.await_all_durable(Duration::from_secs(5)) {
        Ok(())
    } else {
        Err(format!(
            "durability timeout after {op}; write may not survive restart \
             (ROPE_SYNC_DURABILITY=1)"
        ))
    }
}

/// Mandatory sync wait for GDPR erase/untie — Art. 17 is only satisfied
/// once the payload cannot survive a restart.
fn await_durable_required(store: &LedgerStore, op: &str) -> Result<(), String> {
    if store.await_all_durable(Duration::from_secs(5)) {
        Ok(())
    } else {
        Err(format!(
            "durability timeout after {op}; refusing to ack before fsync \
             (GDPR / erasure path)"
        ))
    }
}

fn oes_proof_to_core(proof: rope_crypto::oes::OESProof) -> rope_core::string::OESProof {
    rope_core::string::OESProof {
        generation: proof.generation,
        state_commitment: proof.state_commitment,
        merkle_proof: proof.merkle_proof,
        signature: proof.signature,
    }
}

/// Node-level ledger manager holding all subsystem references
pub struct LedgerManager {
    registry: Arc<StringRegistry>,
    lattice: Arc<StringLattice>,
    store: Arc<LedgerStore>,
    lifecycle: Arc<LedgerLifecycleManager>,
    oes: Arc<OESManager>,
    /// Held for forthcoming attestation paths (per-node provenance on
    /// new ledger entries). Currently retained but not yet read; do not
    /// remove without clearing the deployer-attestation TODO.
    #[allow(dead_code)]
    node_id: NodeId,
    creator_key: PublicKey,
    clock: Arc<ClockManager>,
    config: LifecycleConfig,
    /// Quipu Canon v2.0 Phase 1.4 — memoises OES ledger-key derivation
    /// per `(wallet, generation)`. See
    /// `docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §3.4 and
    /// `crate::oes_key_cache`.
    key_cache: Arc<OESKeyCache>,
}

/// Response for ledger creation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CreateLedgerResponse {
    pub genesis_string_id: String,
    pub wallet_address: String,
    pub oes_generation: u64,
    pub replication_factor: u32,
}

/// Quipu Canon v1.2 — per-kind counts inside [`GlobalStringStats`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KindCount {
    pub strings: usize,
    pub knots: u64,
}

/// Quipu Canon v1.2 — global registry totals served by `rope_globalStats`.
///
/// Invariant: `total_knots >= total_strings` (every string starts with a
/// genesis knot). The `invariant_holds` field is included so external
/// callers and DCScan can assert it client-side.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GlobalStringStats {
    pub total_strings: usize,
    pub total_knots: u64,
    pub by_kind: std::collections::BTreeMap<String, KindCount>,
    pub invariant_holds: bool,
}

/// Response for ledger append
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppendLedgerResponse {
    pub string_id: String,
    pub parent_id: String,
    pub piece_count: u32,
    pub encrypted_size: u64,
    pub oes_generation: u64,
}

/// Response for ledger status query
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerStatusResponse {
    pub wallet_address: String,
    pub genesis_string_id: String,
    pub head_string_id: String,
    pub entry_count: u64,
    pub total_size_bytes: u64,
    pub oes_generation: u64,
    pub is_deleted: bool,
    pub created_at: i64,
    pub last_appended_at: i64,
}

/// Response for ledger erasure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EraseLedgerResponse {
    pub wallet_address: String,
    pub entries_erased: usize,
    pub audit_hash: String,
    pub key_destruction_method: String,
    pub oes_generations_destroyed: Vec<u64>,
}

/// Response for `rope_untieKnot` (canon v1.1 §4.2 — per-knot GDPR primitive).
///
/// Returned to the caller after a single knot on a wallet's string is untied.
/// The wallet's other knots and the string's hash continuity are preserved.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UntieKnotResponse {
    pub wallet_address: String,
    /// The knot's StringId (its position on the cord — preserved as a tombstone)
    pub knot_string_id: String,
    /// Audit hash committing to (string_id || untied_at || reason)
    pub tombstone_audit_hash: String,
    /// Unix timestamp when the knot was untied
    pub untied_at: i64,
    /// Reason class (e.g. "GdprArticle17", "OwnerRequest")
    pub reason: String,
    /// Number of knots remaining active on the wallet's string after the untying
    pub knots_remaining: usize,
    /// Total tombstones (untied knots) on this wallet's string after the untying
    pub tombstones_total: usize,
    /// GDPR / regulator-facing label
    pub gdpr_article: String,
}

/// Response for repatriation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriateResponse {
    pub wallet_address: String,
    pub entries: Vec<RepatriatedEntryResponse>,
    pub total_entries: usize,
    pub total_bytes: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepatriatedEntryResponse {
    pub string_id: String,
    pub sequence: u64,
    pub oes_generation: u64,
    pub encrypted_size: u64,
    pub decrypted_content: Option<Vec<u8>>,
}

impl LedgerManager {
    pub fn new(
        lattice: Arc<StringLattice>,
        store: Arc<LedgerStore>,
        oes: Arc<OESManager>,
        node_id: NodeId,
        creator_key: PublicKey,
        clock: Arc<ClockManager>,
    ) -> Self {
        let config = LifecycleConfig::default();
        Self {
            registry: Arc::new(StringRegistry::new()),
            lattice,
            store,
            lifecycle: Arc::new(LedgerLifecycleManager::new(config.clone())),
            oes,
            node_id,
            creator_key,
            clock,
            config,
            key_cache: Arc::new(OESKeyCache::default()),
        }
    }

    /// Read-only handle to the OES key cache. Exposed for metrics and
    /// for tests; production callers should use `append_to_ledger` and
    /// `repatriate_ledger`, both of which transparently consult the cache.
    pub fn key_cache(&self) -> &OESKeyCache {
        &self.key_cache
    }

    /// Quipu Canon v2.0 Phase 1.6 — rebuild the in-process lattice and
    /// string registry from the persistent store at node boot.
    ///
    /// Call exactly once, immediately after construction, when the
    /// `LedgerStore` was opened via `LedgerStore::open_with_recovery`.
    /// The three inputs are:
    ///
    ///   1. `string_blobs` — every serialised `RopeString` recovered
    ///      from the `strings` CF; deserialised and re-inserted into
    ///      the lattice via the validation-free restore path.
    ///   2. `tombstones` — every canon v1.1 §4.2 untie-tombstone; the
    ///      parent edges they carry re-enable hop-past-tombstone walks.
    ///   3. The store's own descriptor/chain mirrors (already recovered
    ///      by `LedgerStore::open_with_recovery`) — used to rebuild the
    ///      `StringRegistry` with original timestamps and counts.
    ///
    /// Returns `(strings_restored, tombstones_restored, ledgers_restored)`.
    pub fn rehydrate_from_disk(
        &self,
        string_blobs: Vec<([u8; 32], Vec<u8>)>,
        tombstones: Vec<([u8; 32], rope_storage::StoredTombstone)>,
    ) -> (usize, usize, usize) {
        use rope_core::personal_ledger::LedgerDescriptor;

        let mut strings_restored = 0usize;
        for (sid, blob) in string_blobs {
            match bincode::deserialize::<rope_core::string::RopeString>(&blob) {
                Ok(string) => {
                    let restored_id = self.lattice.restore_string(string);
                    if restored_id.as_bytes() != &sid {
                        // The blob's content-derived id no longer matches the
                        // key it was stored under — flag loudly, this means
                        // on-disk tampering or a serialisation format drift.
                        tracing::error!(
                            "ledger rehydrate: restored string id mismatch \
                             (key={}, recomputed={}) — investigate the ledger DB",
                            hex::encode(sid),
                            restored_id.to_hex()
                        );
                    } else {
                        strings_restored += 1;
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "ledger rehydrate: undecodable string blob {} ({}) — skipped",
                        hex::encode(sid),
                        e
                    );
                }
            }
        }

        let mut tombstones_restored = 0usize;
        for (sid, stored) in tombstones {
            let parents = stored
                .parents
                .iter()
                .map(|p| StringId::new(*p))
                .collect::<Vec<_>>();
            self.lattice.restore_tombstone(
                StringId::new(sid),
                rope_core::lattice::KnotTombstone {
                    untied_at: stored.untied_at,
                    audit_hash: stored.audit_hash,
                    reason: stored.reason,
                },
                parents,
            );
            tombstones_restored += 1;
        }

        // Rebuild the registry from the store's recovered descriptor +
        // chain mirrors. All persisted ledgers are wallet-kind today
        // (the store keys by wallet address); other kinds re-register
        // through their own emitters.
        let mut ledgers_restored = 0usize;
        for wallet in self.store.all_wallets() {
            let Some(stored) = self.store.get_descriptor(&wallet) else {
                continue;
            };
            let chain: Vec<StringId> = self
                .store
                .get_chain(&wallet)
                .into_iter()
                .map(StringId::new)
                .collect();
            let descriptor = LedgerDescriptor {
                kind: rope_core::personal_ledger::StringKind::Wallet,
                wallet_address: stored.wallet_address.clone(),
                genesis_string_id: StringId::new(stored.genesis_string_id),
                head_string_id: StringId::new(stored.head_string_id),
                entry_count: stored.entry_count,
                total_size_bytes: stored.total_size_bytes,
                oes_generation_at_creation: stored.oes_generation_at_creation,
                current_oes_generation: stored.current_oes_generation,
                created_at: stored.created_at,
                last_appended_at: stored.last_appended_at,
                is_deleted: stored.is_deleted,
                deleted_at: stored.deleted_at,
                piece_count: 0,
                replication_factor: stored.replication_factor,
            };
            if self.registry.restore_string_state(descriptor, &chain) {
                ledgers_restored += 1;
            }
        }

        if strings_restored + tombstones_restored + ledgers_restored > 0 {
            tracing::info!(
                "Ledger rehydration complete: {} knots, {} tombstones, {} ledgers \
                 restored from disk (Quipu Canon v2.0 Phase 1.6)",
                strings_restored,
                tombstones_restored,
                ledgers_restored
            );
        }

        (strings_restored, tombstones_restored, ledgers_restored)
    }

    /// Phase 1.6.1 (2026-08-11 P1) — metadata-only rehydration.
    ///
    /// Restores tombstones and ledger descriptors from disk, but
    /// **does not** load any knot payloads (RopeString blobs) into
    /// the lattice. Used together with
    /// [`rope_storage::LedgerStore::open_with_recovery_lazy`] to boot
    /// the node without paying the ~4.5 GB / ~5 min eager-rehydration
    /// cost that crash-looped the service on 2026-08-11.
    ///
    /// After this call, individual knots are loaded on demand via
    /// [`Self::ensure_string_loaded`] (typically triggered from within
    /// the lattice `get_string` fast-path in the caller's own RPC
    /// handlers) or by a background task spawned from
    /// [`Self::spawn_background_rehydration`].
    ///
    /// Tombstones are still restored eagerly because the total set is
    /// small (< 200 B each, ~4 today, capped at O(k) where k = untie
    /// rate) and the lattice needs the complete tombstone set at every
    /// read to correctly return `None` for erased knots. Ledger
    /// descriptors are restored eagerly for the same reason: they're
    /// small in aggregate and hot-path readers (rope_getStringStatus,
    /// rope_globalStats) depend on them.
    ///
    /// Returns `(tombstones_restored, ledgers_restored)`.
    pub fn rehydrate_metadata_only(
        &self,
        tombstones: Vec<([u8; 32], rope_storage::StoredTombstone)>,
    ) -> (usize, usize) {
        use rope_core::personal_ledger::LedgerDescriptor;

        let mut tombstones_restored = 0usize;
        for (sid, stored) in tombstones {
            let parents = stored
                .parents
                .iter()
                .map(|p| StringId::new(*p))
                .collect::<Vec<_>>();
            self.lattice.restore_tombstone(
                StringId::new(sid),
                rope_core::lattice::KnotTombstone {
                    untied_at: stored.untied_at,
                    audit_hash: stored.audit_hash,
                    reason: stored.reason,
                },
                parents,
            );
            tombstones_restored += 1;
        }

        let mut ledgers_restored = 0usize;
        for wallet in self.store.all_wallets() {
            let Some(stored) = self.store.get_descriptor(&wallet) else {
                continue;
            };
            let chain: Vec<StringId> = self
                .store
                .get_chain(&wallet)
                .into_iter()
                .map(StringId::new)
                .collect();
            let descriptor = LedgerDescriptor {
                kind: rope_core::personal_ledger::StringKind::Wallet,
                wallet_address: stored.wallet_address.clone(),
                genesis_string_id: StringId::new(stored.genesis_string_id),
                head_string_id: StringId::new(stored.head_string_id),
                entry_count: stored.entry_count,
                total_size_bytes: stored.total_size_bytes,
                oes_generation_at_creation: stored.oes_generation_at_creation,
                current_oes_generation: stored.current_oes_generation,
                created_at: stored.created_at,
                last_appended_at: stored.last_appended_at,
                is_deleted: stored.is_deleted,
                deleted_at: stored.deleted_at,
                piece_count: 0,
                replication_factor: stored.replication_factor,
            };
            if self.registry.restore_string_state(descriptor, &chain) {
                ledgers_restored += 1;
            }
        }

        tracing::info!(
            target: "rope_node::ledger",
            "Lazy rehydration (metadata only): {} tombstones, {} ledgers restored; \
             knot payloads will be loaded on demand",
            tombstones_restored,
            ledgers_restored
        );

        (tombstones_restored, ledgers_restored)
    }

    /// Phase 1.6.1 (2026-08-11 P1) — ensure a single knot's payload is
    /// present in the lattice, loading it from disk on cache miss.
    ///
    /// Returns `true` if the string is now present in the lattice
    /// (either it was already loaded, was just loaded from disk, or
    /// its restore is otherwise unnecessary because it is tombstoned).
    /// Returns `false` if the string is not on disk and never was
    /// (i.e. genuinely unknown).
    ///
    /// This is a no-op fast-path when the string is already in the
    /// lattice — the cost is a single dashmap read on the shard.
    /// On cache miss it performs one RocksDB point-read + one bincode
    /// deserialise, both O(log store) and cheap.
    ///
    /// Callers that iterate over many knots (e.g. `rope_walkString`)
    /// should call this once per id before touching the lattice, so a
    /// lazy-booted node fills its working set exactly to the query
    /// pattern rather than eagerly loading the entire history at boot.
    pub fn ensure_string_loaded(&self, id: &StringId) -> bool {
        // Fast path: already in the lattice (this includes the
        // tombstoned case, because `get_string` returns None for
        // erased knots but the shard still holds the tombstone entry
        // — we treat both "present + live" and "present + tombstoned"
        // as no-op success).
        if self.lattice.get_string(id).is_some() {
            return true;
        }
        // The lattice can also legitimately hold nothing for this id
        // even though it's a live knot — that's the lazy case. Fall
        // through to a disk read.
        let sid_bytes = *id.as_bytes();
        match self.store.read_string_blob(&sid_bytes) {
            Ok(Some(blob)) => match bincode::deserialize::<rope_core::string::RopeString>(&blob) {
                Ok(string) => {
                    let restored_id = self.lattice.restore_string(string);
                    if restored_id.as_bytes() != &sid_bytes {
                        tracing::error!(
                            target: "rope_node::ledger",
                            "ensure_string_loaded: restored id mismatch \
                             (key={}, recomputed={}) — investigate the ledger DB",
                            hex::encode(sid_bytes),
                            restored_id.to_hex()
                        );
                        return false;
                    }
                    true
                }
                Err(e) => {
                    tracing::error!(
                        target: "rope_node::ledger",
                        "ensure_string_loaded: undecodable blob {} ({}) — skipped",
                        hex::encode(sid_bytes),
                        e
                    );
                    false
                }
            },
            Ok(None) => false, // Not on disk — genuinely unknown.
            Err(e) => {
                tracing::warn!(
                    target: "rope_node::ledger",
                    "ensure_string_loaded: disk read failed for {} ({}); \
                     treating as unknown",
                    hex::encode(sid_bytes),
                    e
                );
                false
            }
        }
    }

    /// Phase 1.6.1 (2026-08-11 P1) — prime the in-memory lattice with
    /// every knot on `wallet`'s chain from persistent storage.
    ///
    /// Under lazy rehydration, `LedgerManager` boots with metadata only
    /// — no knot payloads are in the lattice. The lattice walkers
    /// (`walk_ledger_chain`, `walk_string_with_tombstones`) depend on
    /// `shard.strings.get(&id)` returning the live `RopeString`, so a
    /// naive walk against an unloaded chain terminates at the first
    /// missing knot. This helper reads the wallet's persistent chain
    /// mirror (which is authoritative and cheap: just a `Vec<[u8;32]>`
    /// per wallet) and calls `ensure_string_loaded` for every id so a
    /// subsequent walk sees the whole thing.
    ///
    /// Cost: one RocksDB point-read per knot on the chain. For a
    /// 100-knot personal ledger this is ~100 × ~50 µs ≈ 5 ms and is
    /// entirely acceptable for a rope_repatriateLedger / erase_ledger
    /// call. For a 100k-knot ledger it's 5 s, which is orders of
    /// magnitude cheaper than paying for all 100k knots at every node
    /// boot (which is what the eager path did).
    ///
    /// Returns the number of knot payloads that had to be loaded from
    /// disk (i.e. were not already in the lattice).
    pub fn prime_wallet_chain(&self, wallet_bytes: &[u8]) -> usize {
        let mut loaded = 0usize;
        for sid_bytes in self.store.get_chain(wallet_bytes) {
            let id = StringId::new(sid_bytes);
            if self.lattice.get_string(&id).is_none() {
                if self.ensure_string_loaded(&id) {
                    loaded += 1;
                }
            }
        }
        if loaded > 0 {
            tracing::debug!(
                target: "rope_node::ledger",
                "prime_wallet_chain: hydrated {} knot(s) on demand",
                loaded
            );
        }
        loaded
    }

    /// Phase 1.6.1 (2026-08-11 P1) — background rehydration pass.
    ///
    /// Streams every persisted knot payload from disk in fixed-size
    /// batches, restoring each batch into the lattice. Sleeps
    /// `sleep_between_batches` between batches so RSS growth is
    /// spread over time and does not spike past the systemd cgroup
    /// ceiling.
    ///
    /// **This method is synchronous — it blocks the calling thread
    /// until every persisted knot has been streamed.** The RPC
    /// listener has typically already bound to `:8545` before this
    /// runs, so external callers see a working node throughout.
    /// Callers that want fire-and-forget behaviour should wrap this
    /// in `tokio::task::spawn_blocking`.
    ///
    /// Returns the number of knots restored (including any that were
    /// already present from prior `ensure_string_loaded` calls — the
    /// lattice's `restore_string` is idempotent, so racing with
    /// on-demand loads is safe and returns the same total).
    ///
    /// Logs progress at INFO every batch so operators watching
    /// `journalctl -u datachain-rope -f` can see the pass advance.
    pub fn rehydrate_strings_in_background(
        &self,
        batch_size: usize,
        sleep_between_batches: Duration,
    ) -> Result<usize, String> {
        let batch_size = batch_size.max(1);
        let mut batch_index = 0usize;
        let mut total_restored = 0usize;
        let mut total_mismatched = 0usize;
        let start = std::time::Instant::now();

        tracing::info!(
            target: "rope_node::ledger",
            "Background rehydration starting: batch_size={}, sleep_between_batches={}ms",
            batch_size,
            sleep_between_batches.as_millis()
        );

        let streamed = self
            .store
            .stream_string_blobs(batch_size, sleep_between_batches, |batch| {
                batch_index += 1;
                let batch_len = batch.len();
                for (sid, blob) in batch {
                    match bincode::deserialize::<rope_core::string::RopeString>(&blob) {
                        Ok(string) => {
                            let restored_id = self.lattice.restore_string(string);
                            if restored_id.as_bytes() != &sid {
                                total_mismatched += 1;
                                tracing::error!(
                                    target: "rope_node::ledger",
                                    "background rehydrate: restored id mismatch \
                                     (key={}, recomputed={})",
                                    hex::encode(sid),
                                    restored_id.to_hex()
                                );
                            } else {
                                total_restored += 1;
                            }
                        }
                        Err(e) => {
                            total_mismatched += 1;
                            tracing::error!(
                                target: "rope_node::ledger",
                                "background rehydrate: undecodable blob {} ({}) — skipped",
                                hex::encode(sid),
                                e
                            );
                        }
                    }
                }
                tracing::info!(
                    target: "rope_node::ledger",
                    "Background rehydration: batch #{} ({} knots), \
                     cumulative restored={}, mismatched={}, elapsed={}s",
                    batch_index,
                    batch_len,
                    total_restored,
                    total_mismatched,
                    start.elapsed().as_secs()
                );
                Ok(())
            })
            .map_err(|e| format!("stream_string_blobs failed: {}", e))?;

        tracing::info!(
            target: "rope_node::ledger",
            "Background rehydration complete: {} knots streamed, {} restored, \
             {} mismatched, elapsed={}s",
            streamed,
            total_restored,
            total_mismatched,
            start.elapsed().as_secs()
        );
        Ok(total_restored)
    }

    /// Create a new personal ledger for a wallet.
    ///
    /// 1. Derive OES ledger key for this wallet
    /// 2. Build genesis RopeString
    /// 3. Add to lattice
    /// 4. Register in ledger registry
    /// 5. Slice genesis string for distribution
    /// 6. Record creation event
    pub fn create_ledger(&self, wallet_hex: &str) -> Result<CreateLedgerResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        // Quipu Canon v2.0 Phase 1.2: serialise the create ↔ append race.
        // Without this, a concurrent append could observe a partially-
        // initialised registry state. The lock is interned per-wallet so
        // distinct wallets still create in parallel.
        //
        // P1 lattice-metrics (§17.5 #1): wrap the acquisition so wait +
        // hold nanoseconds land in the head_guard_* histograms and the
        // create_ledger per-op counters. Metrics are recorded on Drop.
        let head_lock = self.registry.wallet_head_lock(&wallet_bytes);
        let head_guard = instrument_head_lock(&head_lock, LatticeOp::CreateLedger);

        if self.registry.get_descriptor(&wallet_bytes).is_some() {
            return Err(format!("Ledger already exists for wallet {}", wallet_hex));
        }

        let generation = self.oes.generation();
        let oes_proof = oes_proof_to_core(self.oes.generate_proof());
        // Quipu Canon v2.0 Phase 1.3 — pick the per-wallet shard so two
        // genesis writes for distinct wallets do not contend on one mutex.
        let clock_snapshot = self.clock.tick_for_wallet(&wallet_bytes);
        let replication = self.config.default_replication_factor;

        let genesis_string = build_genesis_string(
            &wallet_bytes,
            self.creator_key.clone(),
            clock_snapshot,
            generation,
            oes_proof,
            replication,
        )
        .map_err(|e| e.to_string())?;

        // Phase 1.6 — serialise the knot payload for the persistent
        // store BEFORE the lattice consumes the string.
        let genesis_blob = bincode::serialize(&genesis_string)
            .map_err(|e| format!("genesis blob serialization: {}", e))?;

        let genesis_id = self
            .lattice
            .add_string(genesis_string)
            .map_err(|e| e.to_string())?;
        self.store
            .put_string_blob(*genesis_id.as_bytes(), genesis_blob)
            .map_err(persist_err)?;

        let desc = self
            .registry
            .create_ledger(&wallet_bytes, genesis_id, generation, replication)
            .map_err(|e| e.to_string())?;

        self.store
            .append_to_chain(&wallet_bytes, *genesis_id.as_bytes())
            .map_err(persist_err)?;
        let stored_desc = rope_storage::ledger_db::StoredLedgerDescriptor {
            wallet_address: wallet_bytes.clone(),
            genesis_string_id: *genesis_id.as_bytes(),
            head_string_id: *genesis_id.as_bytes(),
            entry_count: 1,
            total_size_bytes: 0,
            oes_generation_at_creation: generation,
            current_oes_generation: generation,
            created_at: desc.created_at,
            last_appended_at: desc.last_appended_at,
            is_deleted: false,
            deleted_at: None,
            replication_factor: replication,
        };
        self.store
            .put_descriptor(&wallet_bytes, stored_desc)
            .map_err(persist_err)?;

        self.lifecycle
            .record_creation(wallet_bytes, *genesis_id.as_bytes(), generation);

        // 2026-07-27 P1.2: never wait on Condvar / fsync while holding
        // the per-wallet head lock (stalls every concurrent append).
        drop(head_guard);

        // Phase 1.6 + 2026-07-27 P1: default ack-after-enqueue. Opt into
        // the original "RPC ⇒ fsync'd" contract with ROPE_SYNC_DURABILITY=1.
        await_durable_create_append(&self.store, "create_ledger")?;

        tracing::info!(
            "Created personal ledger for wallet {} — genesis {}",
            wallet_hex,
            genesis_id
        );

        Ok(CreateLedgerResponse {
            genesis_string_id: genesis_id.to_hex(),
            wallet_address: wallet_hex.to_string(),
            oes_generation: generation,
            replication_factor: replication,
        })
    }

    /// Append an interaction to a wallet's ledger.
    ///
    /// 1. Serialize the interaction
    /// 2. Derive OES ledger key for this wallet+generation
    /// 3. Encrypt the content
    /// 4. Wrap in LedgerEnvelope
    /// 5. Build RopeString referencing previous head as parent
    /// 6. Add to lattice
    /// 7. Slice encrypted content into pieces
    /// 8. Update registry + storage
    pub fn append_to_ledger(
        &self,
        wallet_hex: &str,
        interaction: InteractionRecord,
    ) -> Result<AppendLedgerResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        // Phase 1.6.γ (2026-08-12 wedge remediation) — pre-compute the
        // work that does NOT depend on the wallet's head_id BEFORE we
        // take the per-wallet head lock. This shrinks the critical
        // section width so a slow OES cache-miss on one wallet no
        // longer stalls same-wallet appends (and, in combination with
        // per-wallet locks, still lets appends to different wallets
        // execute their pre-work in parallel).
        //
        // Two things are safe to pre-compute:
        //
        //   1. `plaintext = serde_json::to_vec(interaction)` — a pure
        //      function of the caller-supplied `interaction`. No shared
        //      state involved.
        //
        //   2. `key = key_cache.get_or_derive_for_oes(wallet, gen, oes)`
        //      — a pure function of `(wallet, generation)`. The OES
        //      generation is a globally-monotonic counter; we snapshot
        //      it here and re-read it under the lock. Cache hit is
        //      essentially free; cache miss pays the 30-50µs BLAKE3
        //      work OUTSIDE the lock. The cache is thread-safe
        //      (`parking_lot::RwLock<HashMap<...>>`), so concurrent
        //      derivations for the same (wallet, gen) are correct.
        //
        // Correctness under generation rotation: the OES `generation()`
        // read below is best-effort. If a rotation happens between
        // this snapshot and the head_guard acquisition, we detect the
        // drift under the lock and re-derive. Rotation is rare
        // (every ~100 anchors, several minutes), so the fast path is
        // taken essentially always.
        let plaintext =
            serde_json::to_vec(&interaction).map_err(|e| format!("Serialization: {}", e))?;
        let snapshot_generation = self.oes.generation();
        let pre_derived_key =
            self.key_cache
                .get_or_derive_for_oes(&wallet, snapshot_generation, &self.oes);

        // Quipu Canon v2.0 Phase 1.2: per-wallet head lock.
        //
        // The read-build-insert-update sequence below is non-atomic: we
        // read `desc.head_string_id`, build a new RopeString whose parent
        // is that head, insert it into the lattice, and then write the
        // new head back via `record_append`. Two concurrent appends to
        // the SAME wallet without this lock would both read head=X, both
        // build with parent=X, and both insert — forking the wallet's
        // chain in the lattice. The losing knot would be permanently
        // orphaned (still in the lattice, invisible to walk_ledger_chain).
        //
        // The lock is interned per-wallet by `StringRegistry`, so
        // appends to DIFFERENT wallets do not contend. See
        // `EntityHeadLocks` in `rope-core::personal_ledger`.
        //
        // P1 lattice-metrics (§17.5 #1): the append_to_ledger op is the
        // primary contention target under the §17 wedge. Instrumented so
        // we can prove whether Phase C removed the contention or just
        // moved it (per-op mean_wait_ns comparison across deploys).
        let head_lock = self.registry.wallet_head_lock(&wallet_bytes);
        let head_guard = instrument_head_lock(&head_lock, LatticeOp::AppendToLedger);

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!("Ledger deleted for wallet {}", wallet_hex));
        }

        let head_id = desc.head_string_id;
        let generation = self.oes.generation();
        let sequence_number = desc.entry_count;

        // Phase 1.6.δ (2026-08-24 lazy-append fix) — under
        // ROPE_LAZY_REHYDRATE=1 the head knot is registry-metadata-only
        // and is NOT in the in-memory lattice. `add_string` below
        // verifies that every declared parent is present in the lattice;
        // if the head is only on disk it returns `MissingParent(head_id)`
        // and the whole append fails.
        //
        // Every OTHER path that touches the lattice for a wallet's chain
        // (`walk_ledger_chain`, `repatriate_ledger`, `erase_ledger`,
        // `walk_string_with_tombstones`) calls `prime_wallet_chain`
        // before doing lattice work. `append_to_ledger` was the sole
        // path that skipped it — so the first append after a lazy-boot
        // on any wallet whose head had not yet been touched by an
        // unrelated read failed with a spurious "Missing parent string"
        // error. In production this manifested on the system ledgers
        // (`d001` node-requests, `d002` governance, `d003` databox,
        // `d004` revenue-conversion, `d005` admin-tokens) after the
        // 2026-08-24 new-BLUE cutover, where dc-explorer's governance
        // and revenue-conversion append paths failed until each ledger
        // was primed via `rope_repatriatePersonalLedger`.
        //
        // Fix: prime the head knot (depth-1, O(1) lattice check + at
        // most one RocksDB point-read on cache-miss) before we hand a
        // new knot to the lattice. This is the smallest-possible
        // priming that preserves `add_string`'s parent invariant —
        // deeper ancestors are irrelevant because `add_string` only
        // verifies immediate parents (see `lattice::add_string` step
        // 1+2). We deliberately do NOT call `prime_wallet_chain` here
        // because that walks the full chain and pays ~50 µs × N knots
        // per append — on the oracle-agent (C002, 84k knots) that
        // would be ~4 s of RocksDB reads on the hot append path. Full
        // chain priming belongs on the walker RPCs
        // (`rope_repatriatePersonalLedger`, `rope_walkString`), not
        // here.
        //
        // Safe on the genesis sentinel (`[0u8; 32]`):
        // `ensure_string_loaded` returns `false` (not on disk, never
        // was) and `add_string` skips the all-zero parent explicitly
        // (see step 1+2 of `lattice::add_string`), so we never fail
        // on the very first append after `create_personal_ledger`.
        //
        // Runs inside the wallet head lock (`head_guard`), which is
        // deliberate: it serialises priming with the append itself so
        // a concurrent same-wallet append cannot start reading a
        // half-primed lattice. The extra latency inside the critical
        // section is one dashmap probe (fast path) or one RocksDB
        // point-read (~50 µs on cold cache) — negligible against the
        // encryption + serialisation work already inside the lock.
        //
        // Fallback on disk failure: `ensure_string_loaded` returns
        // `false` and logs a warning, then we fall through to
        // `add_string` which surfaces the authentic
        // `MissingParent(head_id)` error to the caller. The caller
        // can then use `rope_repatriatePersonalLedger` to prime the
        // full chain and retry — matching the operational unblock
        // already validated in production 2026-08-24.
        if !head_id.as_bytes().iter().all(|&b| b == 0)
            && !self.ensure_string_loaded(&head_id)
        {
            tracing::warn!(
                target: "rope_node::ledger",
                "append_to_ledger: head {} for wallet {} is neither in \
                 the lattice nor on disk; add_string will surface \
                 MissingParent — caller must prime via \
                 rope_repatriatePersonalLedger and retry",
                head_id,
                wallet_hex
            );
        }

        // Phase 1.6.γ — reuse the pre-derived key from outside the lock
        // when generation is stable (99%+ of the time). Fall back to
        // an inside-lock derivation if OES rotated during the window
        // between snapshot and lock acquisition — still cheap because
        // the cache is a `RwLock<HashMap>` read, but the derive itself
        // now happens inside the lock. Correctness is unchanged: the
        // key is a pure function of (wallet, generation), and we always
        // encrypt with the current generation.
        let key = if generation == snapshot_generation {
            pre_derived_key
        } else {
            self.key_cache
                .get_or_derive_for_oes(&wallet, generation, &self.oes)
        };

        let encrypted =
            encrypt_ledger_content(&key, &plaintext, &wallet, generation, sequence_number)
                .map_err(|e| e.to_string())?;

        let envelope = LedgerEnvelope::encrypted_v1(encrypted);
        let envelope_bytes = envelope.serialize();
        let encrypted_size = envelope_bytes.len() as u64;

        let oes_proof = oes_proof_to_core(self.oes.generate_proof());
        // Quipu Canon v2.0 Phase 1.3 — per-wallet shard tick removes the
        // global Lamport mutex bottleneck on the per-knot append path.
        let clock_snapshot = self.clock.tick_for_wallet(&wallet_bytes);

        let new_string = build_append_string(
            envelope_bytes.clone(),
            head_id,
            self.creator_key.clone(),
            clock_snapshot,
            generation,
            oes_proof,
            self.config.default_replication_factor,
        )
        .map_err(|e| e.to_string())?;

        // Phase 1.6 — serialise the knot payload for the persistent
        // store BEFORE the lattice consumes the string.
        let knot_blob = bincode::serialize(&new_string)
            .map_err(|e| format!("knot blob serialization: {}", e))?;

        let new_id = self
            .lattice
            .add_string(new_string)
            .map_err(|e| e.to_string())?;
        self.store
            .put_string_blob(*new_id.as_bytes(), knot_blob)
            .map_err(persist_err)?;

        let slicing = slice_encrypted_content(&envelope_bytes, self.config.piece_size);
        let piece_count = slicing.pieces.len() as u32;

        self.registry
            .record_append(&wallet_bytes, new_id, encrypted_size, generation)
            .map_err(|e| e.to_string())?;
        self.store
            .append_to_chain(&wallet_bytes, *new_id.as_bytes())
            .map_err(persist_err)?;
        // Phase 1.6 — keep the persisted descriptor in lockstep with the
        // registry so recovery restores accurate head/entry-count/
        // last-appended values, not the creation-time snapshot.
        if let Some(mut stored) = self.store.get_descriptor(&wallet_bytes) {
            stored.head_string_id = *new_id.as_bytes();
            stored.entry_count += 1;
            stored.total_size_bytes += encrypted_size;
            stored.current_oes_generation = generation;
            stored.last_appended_at = chrono::Utc::now().timestamp();
            self.store
                .put_descriptor(&wallet_bytes, stored)
                .map_err(persist_err)?;
        }

        self.lifecycle.record_append(
            wallet_bytes,
            *new_id.as_bytes(),
            *head_id.as_bytes(),
            encrypted_size,
            piece_count,
        );

        // 2026-07-27 P1.2: release head lock before any durability wait.
        drop(head_guard);

        // Phase 1.6 + 2026-07-27 P1: default ack-after-enqueue (see
        // `sync_durability_enabled`). High-volume Quipu appends must not
        // park a blocking thread on every fsync.
        await_durable_create_append(&self.store, "append_to_ledger")?;

        tracing::debug!(
            "Appended to ledger {} — entry {} ({} pieces, {} bytes encrypted)",
            wallet_hex,
            new_id,
            piece_count,
            encrypted_size
        );

        Ok(AppendLedgerResponse {
            string_id: new_id.to_hex(),
            parent_id: head_id.to_hex(),
            piece_count,
            encrypted_size,
            oes_generation: generation,
        })
    }

    /// Anchor a signed deployer attestation onto the deployer's personal
    /// ledger (which lives on the global Datachain Rope lattice — i.e. the
    /// "main Rope ledger" in Quipu Canon parlance).
    ///
    /// The attestation is recorded as an `InteractionType::IdentityClaim`
    /// knot whose payload is the canonical JSON of the attested
    /// `[deployer]` table, plus contextual metadata (node_id, chain_id,
    /// attestation_kind = "deployer_v1"). The deployer's personal ledger
    /// is auto-created if it does not yet exist.
    ///
    /// Idempotency is the caller's responsibility — the manager itself
    /// will append every time. Use the marker-file pattern in `node.rs` or
    /// the `force` flag on the RPC method to control re-anchoring.
    pub fn anchor_deployer_attestation(
        &self,
        wallet_hex: &str,
        attestation_canonical: &[u8],
        self_signature_hex: &str,
        attesting_node_id_hex: &str,
        chain_id: u64,
    ) -> Result<AppendLedgerResponse, String> {
        use rope_core::personal_ledger::{InteractionRecord, InteractionType};

        // Auto-create the ledger if this wallet has never anchored before.
        if self
            .registry
            .get_descriptor(
                &WalletAddress::from_hex(wallet_hex)
                    .map_err(|e| e.to_string())?
                    .as_bytes()
                    .to_vec(),
            )
            .is_none()
        {
            let _ = self.create_ledger(wallet_hex)?;
        }

        let mut metadata = hashbrown::HashMap::new();
        metadata.insert("attestation_kind".to_string(), "deployer_v1".to_string());
        metadata.insert("self_signature".to_string(), self_signature_hex.to_string());
        metadata.insert(
            "attesting_node_id".to_string(),
            attesting_node_id_hex.to_string(),
        );
        metadata.insert("chain_id".to_string(), chain_id.to_string());

        let record = InteractionRecord {
            interaction_type: InteractionType::IdentityClaim,
            counterparty: None,
            data: attestation_canonical.to_vec(),
            timestamp: chrono::Utc::now().timestamp(),
            metadata,
        };

        let resp = self.append_to_ledger(wallet_hex, record)?;

        tracing::info!(
            "Anchored deployer attestation: wallet={} string_id={} \
             attesting_node={} sig={}…",
            wallet_hex,
            resp.string_id,
            attesting_node_id_hex,
            &self_signature_hex.chars().take(16).collect::<String>()
        );

        Ok(resp)
    }

    // ------------------------------------------------------------------
    // Quipu Canon v1.2 — generic-string API. Wallets are still the
    // common case; smart contracts, tokenized assets, DIDs, and the
    // global cord land here too. The wallet-specific methods above
    // remain intact for backward compat.
    // ------------------------------------------------------------------

    /// List all strings of a given kind (or all kinds when `kind` is
    /// `None`), paginated. Returns `(total_strings, slice)` so the
    /// caller can render pagination without a second round-trip.
    pub fn list_strings(
        &self,
        kind: Option<StringKind>,
        offset: usize,
        limit: usize,
    ) -> (usize, Vec<rope_core::personal_ledger::LedgerDescriptor>) {
        let mut all: Vec<_> = match kind {
            Some(k) => self.registry.descriptors_by_kind(k),
            None => {
                let mut acc = Vec::new();
                for k in [
                    StringKind::Cord,
                    StringKind::Wallet,
                    StringKind::Contract,
                    StringKind::Asset,
                    StringKind::Did,
                ] {
                    acc.extend(self.registry.descriptors_by_kind(k));
                }
                acc
            }
        };
        // Stable order: most-recently-anchored first.
        all.sort_by(|a, b| b.last_appended_at.cmp(&a.last_appended_at));
        let total = all.len();
        let slice: Vec<_> = all.into_iter().skip(offset).take(limit).collect();
        (total, slice)
    }

    /// Look up one string by `(kind, hex_id)`. Hex id may be `0x`-prefixed
    /// or bare; falls back to a wallet lookup when `kind` is `None`.
    pub fn get_string(
        &self,
        kind: Option<StringKind>,
        hex_id: &str,
    ) -> Option<rope_core::personal_ledger::LedgerDescriptor> {
        let stripped = hex_id.trim_start_matches("0x");
        let bytes = hex::decode(stripped).ok()?;
        match kind {
            Some(k) => self.registry.get_string(k, &bytes),
            None => self.registry.get_string(StringKind::Wallet, &bytes),
        }
    }

    /// Quipu Canon v1.2 invariant snapshot: total strings across all
    /// kinds and total knots summed across all strings. By construction,
    /// `total_knots >= total_strings` (every string starts with a
    /// genesis knot).
    pub fn global_stats(&self) -> GlobalStringStats {
        let counts = self.registry.counts_by_kind();
        let mut by_kind = std::collections::BTreeMap::new();
        let mut total_strings = 0usize;
        let mut total_knots = 0u64;
        for (kind, (s, k)) in counts {
            by_kind.insert(
                kind.as_str().to_string(),
                KindCount {
                    strings: s,
                    knots: k,
                },
            );
            total_strings += s;
            total_knots += k;
        }
        GlobalStringStats {
            total_strings,
            total_knots,
            by_kind,
            invariant_holds: total_knots >= total_strings as u64,
        }
    }

    /// Get the status of a wallet's ledger
    pub fn get_ledger_status(&self, wallet_hex: &str) -> Result<LedgerStatusResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        Ok(LedgerStatusResponse {
            wallet_address: wallet_hex.to_string(),
            genesis_string_id: desc.genesis_string_id.to_hex(),
            head_string_id: desc.head_string_id.to_hex(),
            entry_count: desc.entry_count,
            total_size_bytes: desc.total_size_bytes,
            oes_generation: desc.current_oes_generation,
            is_deleted: desc.is_deleted,
            created_at: desc.created_at,
            last_appended_at: desc.last_appended_at,
        })
    }

    /// Erase a wallet's ledger by destroying the OES key link.
    ///
    /// 1. Verify the requester owns the wallet
    /// 2. Mark all strings in the chain as erased in the lattice
    /// 3. Record the OES generations that were in use (for audit)
    /// 4. Mark ledger as deleted in registry
    /// 5. Create erasure audit record
    /// 6. Emit deletion event
    pub fn erase_ledger(
        &self,
        wallet_hex: &str,
        reason: DeletionReason,
    ) -> Result<EraseLedgerResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        // Quipu Canon v2.0 Phase 1.2: hold the per-wallet head lock
        // across mutation + enqueue so a concurrent append cannot land
        // after `walk_ledger_chain` and before `mark_deleted`. Released
        // before the durability Condvar wait (2026-07-27 P1.2).
        //
        // P1 lattice-metrics (§17.5 #1): GDPR paths generally hold the
        // lock much longer than append (chain walk + N delete_string_blob
        // enqueues); instrumented so a spike in erase_ledger hold time
        // does not get misdiagnosed as an append-side regression.
        let head_lock = self.registry.wallet_head_lock(&wallet_bytes);
        let head_guard = instrument_head_lock(&head_lock, LatticeOp::EraseLedger);

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!("Ledger already deleted for {}", wallet_hex));
        }

        // Phase 1.6.1: under lazy rehydration the chain knots may not
        // be in the lattice yet — prime them from disk so the walk
        // below sees the whole ledger.
        self.prime_wallet_chain(&wallet_bytes);

        let chain = self.lattice.walk_ledger_chain(&desc.head_string_id);

        let oes_generations: Vec<u64> = chain
            .iter()
            .filter_map(|id| self.lattice.get_string(id))
            .map(|s| s.oes_generation())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let mut erased_count = 0;
        for id in &chain {
            if self.lattice.mark_erased(*id).is_ok() {
                erased_count += 1;
                // Phase 1.6 — cryptographic erasure must reach disk too:
                // delete the persisted knot payload so it cannot be
                // resurrected by a restart.
                self.store
                    .delete_string_blob(*id.as_bytes())
                    .map_err(persist_err)?;
            }
        }

        self.registry
            .mark_deleted(&wallet_bytes)
            .map_err(|e| e.to_string())?;
        self.store.mark_deleted(&wallet_bytes).map_err(persist_err)?;
        // Enqueues above ran under the head lock (after any prior append
        // enqueue for this wallet). Safe to release before fsync wait.
        drop(head_guard);
        // Phase 1.6 — GDPR erasure is only real once the blob deletions
        // are fsync'd. Always sync; timeout is a hard error (never
        // ack-after-enqueue on this path).
        await_durable_required(&self.store, "erase_ledger")?;

        // Quipu Canon v2.0 Phase 1.4 — drop every cached OES key for
        // this wallet. The Arc<LedgerKey> values zeroize on drop, so
        // erasure removes the keys from process memory as well as
        // marking the ledger deleted in the registry.
        let cached_keys_dropped = self.key_cache.invalidate_wallet(&wallet);
        if cached_keys_dropped > 0 {
            tracing::debug!(
                "Erased {} cached OES key(s) for wallet {}",
                cached_keys_dropped,
                wallet_hex
            );
        }

        let key_method = "oes_evolution";
        let mut audit = LedgerErasureAudit::new(wallet_bytes.clone(), reason);
        audit.complete(erased_count, key_method, oes_generations.clone());

        let audit_hash = hex::encode(audit.audit_hash);

        self.lifecycle.record_deletion(audit);

        tracing::info!(
            "Erased ledger for wallet {} — {} entries, {} OES generations destroyed",
            wallet_hex,
            erased_count,
            oes_generations.len()
        );

        Ok(EraseLedgerResponse {
            wallet_address: wallet_hex.to_string(),
            entries_erased: erased_count,
            audit_hash,
            key_destruction_method: key_method.to_string(),
            oes_generations_destroyed: oes_generations,
        })
    }

    /// Untie a single knot on a wallet's string (Quipu Canon v1.1 §4.2).
    ///
    /// This is the granular GDPR Article 17 primitive — the surgical
    /// alternative to `erase_ledger` (which closes the entire wallet).
    /// Steps:
    ///
    ///   1. Resolve the wallet's chain and verify the target knot belongs to it.
    ///   2. Refuse to untie the genesis knot (would orphan the chain) or a
    ///      ledger that is already wholly deleted.
    ///   3. Call `lattice.mark_knot_untied(knot_id, reason)` — destroys the
    ///      payload + records canonical tombstone metadata.
    ///   4. Return the audit proof (tombstone_audit_hash, untied_at) plus
    ///      transparency counters for the caller / DCScan UI.
    ///
    /// The wallet's other knots, balances, attestations, deeds, and credentials
    /// are unaffected. Walking the string with `walk_string_with_tombstones`
    /// will return the untied knot as `LedgerEntry::Tombstone`.
    pub fn untie_knot(
        &self,
        wallet_hex: &str,
        knot_string_id_hex: &str,
        reason: &str,
    ) -> Result<UntieKnotResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        // Quipu Canon v2.0 Phase 1.2: per-wallet head lock so untie does
        // not race with concurrent appends or erases on the same wallet.
        // Read-only paths (`repatriate_ledger`,
        // `walk_string_with_tombstones`) intentionally skip this lock —
        // a slightly stale snapshot is acceptable for audit views and
        // keeps read scalability high.
        //
        // P1 lattice-metrics (§17.5 #1): untie is rare but very expensive
        // when it hits (chain walk + blob delete + tombstone put + fsync).
        // Instrumented so ad-hoc untie storms are attributable.
        let head_lock = self.registry.wallet_head_lock(&wallet_bytes);
        let head_guard = instrument_head_lock(&head_lock, LatticeOp::UntieKnot);

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!(
                "Ledger already wholly deleted for {} — no knots to untie",
                wallet_hex
            ));
        }

        let knot_id = StringId::from_hex(knot_string_id_hex)
            .map_err(|e| format!("Invalid knot string_id: {}", e))?;

        // Refuse to untie the genesis knot — that would orphan the chain.
        // The wallet-closure pathway is `erase_ledger`, not this one.
        if knot_id == desc.genesis_string_id {
            return Err(format!(
                "Cannot untie the genesis knot on wallet {}. To close the wallet entirely, use rope_erasePersonalLedger (canon §6).",
                wallet_hex
            ));
        }

        // Walk the chain (tombstone-aware) and verify the target belongs to this wallet's string.
        let entries_before = self
            .lattice
            .walk_string_with_tombstones(&desc.head_string_id);
        let belongs = entries_before.iter().any(|e| e.string_id() == knot_id);
        if !belongs {
            return Err(format!(
                "Knot {} does not belong to wallet {}'s string",
                knot_string_id_hex, wallet_hex
            ));
        }

        // Refuse to untie an already-tombstoned knot (idempotency guard).
        if self.lattice.is_knot_untied(&knot_id) {
            return Err(format!(
                "Knot {} is already untied (tombstone exists)",
                knot_string_id_hex
            ));
        }

        // Phase 1.6 — capture the knot's parent edges BEFORE the payload
        // is destroyed. The persisted tombstone carries them so a
        // post-restart lattice can still hop past the deliberate absence.
        let knot_parents: Vec<[u8; 32]> = self
            .lattice
            .get_string(&knot_id)
            .map(|s| s.parentage().iter().map(|p| *p.as_bytes()).collect())
            .unwrap_or_default();

        // Perform the cryptographic erasure + tombstone recording.
        let tombstone = self
            .lattice
            .mark_knot_untied(knot_id, reason)
            .map_err(|e| format!("mark_knot_untied failed: {}", e))?;

        // Phase 1.6 — mirror the erasure + tombstone to disk in the same
        // flush wave, then block until fsync'd. GDPR Art. 17 is only
        // satisfied once the payload cannot survive a restart — always
        // sync; timeout is a hard error.
        self.store
            .delete_string_blob(*knot_id.as_bytes())
            .map_err(persist_err)?;
        self.store
            .put_tombstone(
                *knot_id.as_bytes(),
                rope_storage::StoredTombstone {
                    untied_at: tombstone.untied_at,
                    audit_hash: tombstone.audit_hash,
                    reason: tombstone.reason.clone(),
                    parents: knot_parents,
                },
            )
            .map_err(persist_err)?;
        // Enqueues done under head lock; release before Condvar wait.
        drop(head_guard);
        await_durable_required(&self.store, "untie_knot")?;

        // Recompute counts after the untying for the response.
        let entries_after = self
            .lattice
            .walk_string_with_tombstones(&desc.head_string_id);
        let knots_remaining = entries_after.iter().filter(|e| !e.is_tombstone()).count();
        let tombstones_total = entries_after.iter().filter(|e| e.is_tombstone()).count();

        tracing::info!(
            "Untied knot {} on wallet {} — reason={}, audit={}",
            knot_id,
            wallet_hex,
            reason,
            hex::encode(tombstone.audit_hash)
        );

        Ok(UntieKnotResponse {
            wallet_address: wallet_hex.to_string(),
            knot_string_id: knot_id.to_hex(),
            tombstone_audit_hash: hex::encode(tombstone.audit_hash),
            untied_at: tombstone.untied_at,
            reason: tombstone.reason,
            knots_remaining,
            tombstones_total,
            gdpr_article: "Article 17 — Right to Erasure (per-knot, canon v1.1)".to_string(),
        })
    }

    /// Walk a wallet's string with tombstone awareness (canon v1.1 §6(2)).
    ///
    /// Returns the canonical String → Knot[] hierarchy that powers DCScan's
    /// personal-ledger view: each entry is either an `Active` knot (live
    /// payload) or a `Tombstone` knot (untied, payload destroyed, audit hash
    /// and timestamp preserved). Genesis-first ordering.
    pub fn walk_string_with_tombstones(
        &self,
        wallet_hex: &str,
    ) -> Result<(String, Vec<rope_core::lattice::LedgerEntry>), String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        // Phase 1.6.1: prime chain from disk for lazy-boot correctness.
        self.prime_wallet_chain(&wallet_bytes);

        let entries = self
            .lattice
            .walk_string_with_tombstones(&desc.head_string_id);

        Ok((desc.genesis_string_id.to_hex(), entries))
    }

    /// Repatriate a wallet's ledger — reconstruct from lattice/storage.
    ///
    /// In the full BitTorrent model, this would fetch pieces from remote nodes
    /// via RDP swarms. For now, we reconstruct from the local lattice + storage,
    /// which is the correct behavior for the node that holds the pieces.
    ///
    /// The wallet can then decrypt each entry using its OES key.
    pub fn repatriate_ledger(
        &self,
        wallet_hex: &str,
        decrypt: bool,
    ) -> Result<RepatriateResponse, String> {
        let wallet = WalletAddress::from_hex(wallet_hex).map_err(|e| e.to_string())?;
        let wallet_bytes = wallet.as_bytes().to_vec();

        let desc = self
            .registry
            .get_descriptor(&wallet_bytes)
            .ok_or_else(|| format!("No ledger for wallet {}", wallet_hex))?;

        if desc.is_deleted {
            return Err(format!("Ledger deleted for wallet {}", wallet_hex));
        }

        // Phase 1.6.1: prime chain from disk for lazy-boot correctness.
        self.prime_wallet_chain(&wallet_bytes);

        let chain = self.lattice.walk_ledger_chain(&desc.head_string_id);

        let mut entries = Vec::new();
        let mut total_bytes: u64 = 0;

        for (seq, string_id) in chain.iter().enumerate() {
            let rope_string = match self.lattice.get_string(string_id) {
                Some(s) => s,
                None => continue,
            };

            let content = rope_string.content();
            let content_size = content.len() as u64;
            total_bytes += content_size;

            let decrypted_content = if decrypt && seq > 0 {
                match LedgerEnvelope::deserialize(&content) {
                    Ok(envelope) => match &envelope.payload {
                        rope_crypto::ledger_encryption::LedgerEnvelopePayload::EncryptedV1(
                            encrypted,
                        ) => {
                            // Read path also consults the OES key cache —
                            // repatriating a long ledger reuses the same
                            // key for every entry within an OES generation
                            // (Quipu Canon v2.0 Phase 1.4).
                            let key = self.key_cache.get_or_derive_for_oes(
                                &wallet,
                                encrypted.oes_generation,
                                &self.oes,
                            );
                            decrypt_ledger_content(&key, encrypted).ok()
                        }
                        rope_crypto::ledger_encryption::LedgerEnvelopePayload::Plaintext(data) => {
                            Some(data.clone())
                        }
                    },
                    Err(_) => None,
                }
            } else {
                None
            };

            entries.push(RepatriatedEntryResponse {
                string_id: string_id.to_hex(),
                sequence: seq as u64,
                oes_generation: rope_string.oes_generation(),
                encrypted_size: content_size,
                decrypted_content,
            });
        }

        let total_entries = entries.len();

        Ok(RepatriateResponse {
            wallet_address: wallet_hex.to_string(),
            entries,
            total_entries,
            total_bytes,
        })
    }

    pub fn registry(&self) -> &StringRegistry {
        &self.registry
    }

    pub fn lifecycle(&self) -> &LedgerLifecycleManager {
        &self.lifecycle
    }

    pub fn store(&self) -> &LedgerStore {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rope_core::personal_ledger::InteractionType;

    #[test]
    fn sync_durability_env_defaults_to_ack_after_enqueue() {
        assert!(!parse_sync_durability(None));
        assert!(!parse_sync_durability(Some("")));
        assert!(!parse_sync_durability(Some("0")));
        assert!(!parse_sync_durability(Some("false")));
        assert!(parse_sync_durability(Some("1")));
        assert!(parse_sync_durability(Some("true")));
        assert!(parse_sync_durability(Some("YES")));
        assert!(parse_sync_durability(Some(" on ")));
    }

    fn make_test_manager() -> LedgerManager {
        let lattice = Arc::new(StringLattice::new());
        let store = Arc::new(LedgerStore::new());
        let oes = Arc::new(OESManager::genesis(&[0u8; 32]));
        let node_id = NodeId::new([1u8; 32]);
        let creator_key = PublicKey::from_ed25519([2u8; 32]);
        let clock = Arc::new(ClockManager::new(node_id));
        LedgerManager::new(lattice, store, oes, node_id, creator_key, clock)
    }

    /// Phase 1.6 — build a manager on a persistent store at `path`,
    /// replaying whatever the store recovered from disk. Mirrors the
    /// production boot path in `node.rs`.
    fn make_persistent_manager(path: &std::path::Path) -> LedgerManager {
        let (store, blobs, tombstones) =
            LedgerStore::open_with_recovery(path).expect("open persistent ledger store");
        let lattice = Arc::new(StringLattice::new());
        let oes = Arc::new(OESManager::genesis(&[0u8; 32]));
        let node_id = NodeId::new([1u8; 32]);
        let creator_key = PublicKey::from_ed25519([2u8; 32]);
        let clock = Arc::new(ClockManager::new(node_id));
        let manager = LedgerManager::new(
            lattice,
            Arc::new(store),
            oes,
            node_id,
            creator_key,
            clock,
        );
        manager.rehydrate_from_disk(blobs, tombstones);
        manager
    }

    /// Phase 1.6.1 — same as `make_persistent_manager` but opens the
    /// store lazily and only replays tombstone/ledger metadata. Knot
    /// payloads must be loaded on demand via `ensure_string_loaded`
    /// or `prime_wallet_chain`.
    fn make_lazy_persistent_manager(path: &std::path::Path) -> LedgerManager {
        let (store, blobs, tombstones) =
            LedgerStore::open_with_recovery_lazy(path).expect("open lazy ledger store");
        assert!(
            blobs.is_empty(),
            "lazy open must NOT preload knot blobs into RAM"
        );
        let lattice = Arc::new(StringLattice::new());
        let oes = Arc::new(OESManager::genesis(&[0u8; 32]));
        let node_id = NodeId::new([1u8; 32]);
        let creator_key = PublicKey::from_ed25519([2u8; 32]);
        let clock = Arc::new(ClockManager::new(node_id));
        let manager = LedgerManager::new(
            lattice,
            Arc::new(store),
            oes,
            node_id,
            creator_key,
            clock,
        );
        manager.rehydrate_metadata_only(tombstones);
        manager
    }

    /// Smoke test: sequential create + 2 appends finish in well under a second.
    /// If this hangs, the head-lock wiring is broken at the single-thread
    /// level. If only the multi-threaded tests below hang, the issue is
    /// concurrency-specific.
    #[test]
    fn sequential_create_and_append_smoke_test() {
        let manager = make_test_manager();
        let wallet_hex = "0xdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
        manager.create_ledger(wallet_hex).unwrap();
        manager
            .append_to_ledger(wallet_hex, make_test_interaction(1))
            .unwrap();
        manager
            .append_to_ledger(wallet_hex, make_test_interaction(2))
            .unwrap();
        let wallet_bytes = WalletAddress::from_hex(wallet_hex)
            .unwrap()
            .as_bytes()
            .to_vec();
        let desc = manager.registry.get_descriptor(&wallet_bytes).unwrap();
        assert_eq!(desc.entry_count, 3, "1 genesis + 2 appends");
    }

    fn make_test_interaction(seq: u32) -> InteractionRecord {
        let mut metadata = hashbrown::HashMap::new();
        metadata.insert("seq".to_string(), seq.to_string());
        InteractionRecord {
            interaction_type: InteractionType::IdentityClaim,
            counterparty: None,
            data: format!("interaction-{seq}").into_bytes(),
            timestamp: chrono::Utc::now().timestamp(),
            metadata,
        }
    }

    /// Quipu Canon v2.0 Phase 1.2 — chain-fork race fix.
    ///
    /// Spawns N threads that all call `append_to_ledger` against the same
    /// wallet. With v1.x the read-build-insert-update sequence was not
    /// atomic, so concurrent appends would fork the chain in the lattice
    /// and silently lose all but the winner. With per-wallet locking the
    /// final state must satisfy:
    ///
    ///   - registry.entry_count == 1 (genesis) + N (appended)
    ///   - lattice.walk_ledger_chain(head).len() == 1 + N
    ///   - every appended StringId appears exactly once in the chain
    #[test]
    fn concurrent_appends_to_same_wallet_do_not_fork_the_chain() {
        use std::sync::Arc;
        use std::thread;

        let manager = Arc::new(make_test_manager());
        let wallet_hex = "0x1111111111111111111111111111111111111111";

        manager.create_ledger(wallet_hex).unwrap();

        // 4 threads × 2 appends gives a strong signal that the lock
        // serialises a meaningful interleave window without making the
        // test slow. Each append still does the full encrypt + slice +
        // sign + lattice insert (and on this branch — without the OES
        // key cache from P1.4 — that's all uncached), so the per-append
        // cost dominates. Eight or more appends per wallet make the
        // suite needlessly slow.
        const NUM_THREADS: usize = 4;
        const APPENDS_PER_THREAD: u32 = 2;
        const TOTAL_APPENDS: u64 = (NUM_THREADS as u64) * (APPENDS_PER_THREAD as u64);

        let mut handles = Vec::new();
        for tid in 0..NUM_THREADS {
            let manager = manager.clone();
            handles.push(thread::spawn(move || {
                let mut appended_ids = Vec::new();
                for i in 0..APPENDS_PER_THREAD {
                    let seq = (tid as u32) * 1000 + i;
                    let resp = manager
                        .append_to_ledger(wallet_hex, make_test_interaction(seq))
                        .expect("append must succeed under per-wallet lock");
                    appended_ids.push(resp.string_id);
                }
                appended_ids
            }));
        }

        let mut all_ids = Vec::new();
        for h in handles {
            all_ids.extend(h.join().unwrap());
        }
        assert_eq!(all_ids.len(), TOTAL_APPENDS as usize);

        // Registry-level invariant: entry_count == 1 (genesis) + N (appends)
        let wallet_bytes = WalletAddress::from_hex(wallet_hex)
            .unwrap()
            .as_bytes()
            .to_vec();
        let desc = manager
            .registry
            .get_descriptor(&wallet_bytes)
            .expect("descriptor must exist");
        assert_eq!(
            desc.entry_count,
            1 + TOTAL_APPENDS,
            "registry.entry_count must equal 1 genesis + {TOTAL_APPENDS} appends — \
             a mismatch here means the registry update was preempted by a racing \
             append. With per-wallet locking this MUST hold."
        );

        // Lattice-level invariant: walking the chain from head returns
        // exactly 1 + N knots, none orphaned. Without the head lock the
        // chain would fork and the walk would short-circuit at the first
        // surviving branch.
        let chain = manager.lattice.walk_ledger_chain(&desc.head_string_id);
        assert_eq!(
            chain.len() as u64,
            1 + TOTAL_APPENDS,
            "walk_ledger_chain must return exactly 1 + {TOTAL_APPENDS} knots; \
             a shorter chain proves a knot was orphaned by a concurrent fork."
        );

        // Every appended id appears exactly once in the chain.
        use std::collections::HashSet;
        let chain_hex: HashSet<String> = chain.iter().map(|id| id.to_hex()).collect();
        for id in &all_ids {
            assert!(
                chain_hex.contains(id),
                "appended knot {} missing from final chain — it was orphaned",
                id
            );
        }
    }

    /// Sanity check: appends to DIFFERENT wallets do NOT serialise on each
    /// other's head locks. This is the whole point of per-entity locking
    /// over a global lock — without it Phase 1.2 would just be a global
    /// mutex with extra steps.
    ///
    /// **Scope note:** Each thread does exactly ONE append. Multiple
    /// concurrent appends across distinct wallets in this test would
    /// trigger a pre-existing latent deadlock in `StringLattice` on
    /// `main`: `update_finality` holds `anchors.read()` while invoking
    /// `count_anchor_references`, which itself takes `anchors.read()`,
    /// and parking_lot's `RwLock::read()` is not safe to call recursively
    /// when a third thread has a pending writer. The Quipu Canon v2.0
    /// Phase 1.1 sharded lattice (`feat/v2-phase1-sharded-lattice`)
    /// rewrites `update_finality` so the outer read is dropped before
    /// the inner counts run, eliminating that latent bug. Once both P1.1
    /// and P1.2 merge to `main`, this test can be ramped up to multiple
    /// appends per wallet to additionally stress the lattice path.
    #[test]
    fn concurrent_appends_to_distinct_wallets_each_succeed_independently() {
        use std::sync::Arc;
        use std::thread;

        let manager = Arc::new(make_test_manager());

        const NUM_WALLETS: usize = 4;
        const APPENDS_PER_WALLET: u32 = 1;

        // Pre-create all wallets sequentially.
        let wallets: Vec<String> = (0..NUM_WALLETS)
            .map(|i| format!("0x{:040x}", 0xA0_u64 + i as u64))
            .collect();
        for w in &wallets {
            manager.create_ledger(w).unwrap();
        }

        // Hammer each wallet from its own thread.
        let mut handles = Vec::new();
        for w in wallets.clone() {
            let manager = manager.clone();
            handles.push(thread::spawn(move || {
                for i in 0..APPENDS_PER_WALLET {
                    manager
                        .append_to_ledger(&w, make_test_interaction(i))
                        .unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // Verify each wallet's chain has exactly 1 + APPENDS_PER_WALLET knots.
        for w in &wallets {
            let wallet_bytes = WalletAddress::from_hex(w).unwrap().as_bytes().to_vec();
            let desc = manager.registry.get_descriptor(&wallet_bytes).unwrap();
            assert_eq!(
                desc.entry_count,
                1 + APPENDS_PER_WALLET as u64,
                "wallet {} must have exactly 1 + {} entries",
                w,
                APPENDS_PER_WALLET
            );
        }

        // Lock pool should now hold exactly NUM_WALLETS interned locks.
        assert_eq!(
            manager.registry.head_lock_count(),
            NUM_WALLETS,
            "head-lock pool must hold exactly one Arc per wallet"
        );
    }

    /// Quipu Canon v2.0 Phase 1.6 — the headline guarantee: a ledger
    /// created and appended to before a "restart" (drop + reopen of the
    /// persistent store) is fully readable afterwards, including
    /// decryptable payloads (OES keys are derived from the identity
    /// seed, which is itself persistent in production).
    #[test]
    fn ledgers_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger_db");
        let wallet_hex = "0x000000000000000000000000000000000000c001";

        // ----- first process lifetime -----
        {
            let manager = make_persistent_manager(&db_path);
            manager.create_ledger(wallet_hex).unwrap();
            manager
                .append_to_ledger(wallet_hex, make_test_interaction(1))
                .unwrap();
            manager
                .append_to_ledger(wallet_hex, make_test_interaction(2))
                .unwrap();
            assert!(
                manager
                    .store
                    .await_all_durable(std::time::Duration::from_secs(10)),
                "writes must fsync before the simulated crash"
            );
        } // manager dropped == process died

        // ----- second process lifetime -----
        let manager = make_persistent_manager(&db_path);
        let wallet_bytes = WalletAddress::from_hex(wallet_hex)
            .unwrap()
            .as_bytes()
            .to_vec();
        let desc = manager
            .registry
            .get_descriptor(&wallet_bytes)
            .expect("descriptor must survive restart");
        assert_eq!(desc.entry_count, 3, "1 genesis + 2 appends after restart");
        assert!(!desc.is_deleted);

        // The chain must walk end-to-end through the restored lattice.
        let chain = manager.lattice.walk_ledger_chain(&desc.head_string_id);
        assert_eq!(chain.len(), 3, "full chain must be walkable after restart");

        // Payloads must decrypt — OES generation-0 keys re-derive
        // identically from the same identity seed.
        let repatriated = manager.repatriate_ledger(wallet_hex, true).unwrap();
        assert_eq!(repatriated.total_entries, 3);
        for entry in repatriated.entries.iter().skip(1) {
            let plain = entry
                .decrypted_content
                .as_ref()
                .expect("appended entries must decrypt after restart");
            let text = String::from_utf8_lossy(plain);
            assert!(
                text.contains("IdentityClaim") && text.contains("\"seq\""),
                "decrypted payload must round-trip, got: {text}"
            );
        }

        // And the ledger must still accept new appends post-restart.
        manager
            .append_to_ledger(wallet_hex, make_test_interaction(3))
            .unwrap();
        let desc = manager.registry.get_descriptor(&wallet_bytes).unwrap();
        assert_eq!(desc.entry_count, 4);
    }

    /// Phase 1.6 — GDPR untie survives restart: the tombstone is still
    /// present, the payload is still gone (cryptographic erasure holds
    /// on disk), and the string remains walkable past the absence.
    #[test]
    fn untie_tombstone_survives_restart_and_payload_stays_dead() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger_db");
        let wallet_hex = "0x000000000000000000000000000000000000c002";

        let untied_knot_hex;
        {
            let manager = make_persistent_manager(&db_path);
            manager.create_ledger(wallet_hex).unwrap();
            let victim = manager
                .append_to_ledger(wallet_hex, make_test_interaction(10))
                .unwrap();
            manager
                .append_to_ledger(wallet_hex, make_test_interaction(11))
                .unwrap();
            manager
                .untie_knot(wallet_hex, &victim.string_id, "gdpr-art17-test")
                .unwrap();
            untied_knot_hex = victim.string_id;
            assert!(manager
                .store
                .await_all_durable(std::time::Duration::from_secs(10)));
        }

        let manager = make_persistent_manager(&db_path);

        // The tombstone survives and the walk hops past it.
        let (_genesis, entries) = manager.walk_string_with_tombstones(wallet_hex).unwrap();
        assert_eq!(entries.len(), 3, "genesis + tombstone + live knot");
        let tombstones: Vec<_> = entries.iter().filter(|e| e.is_tombstone()).collect();
        assert_eq!(tombstones.len(), 1, "exactly one tombstone after restart");
        assert_eq!(
            tombstones[0].string_id().to_hex(),
            untied_knot_hex,
            "the tombstone must sit at the untied knot's position"
        );

        // Cryptographic erasure holds: the payload is not in the lattice…
        let untied_id = StringId::from_hex(&untied_knot_hex).unwrap();
        assert!(
            manager.lattice.get_string(&untied_id).is_none(),
            "untied knot payload must NOT reappear after restart"
        );
        // …and repatriation never returns the untied knot's payload.
        let repatriated = manager.repatriate_ledger(wallet_hex, true).unwrap();
        assert!(
            repatriated
                .entries
                .iter()
                .all(|e| e.string_id != untied_knot_hex),
            "the untied knot's payload must never be repatriated"
        );

        // Double-untie is still refused (idempotency guard persisted).
        let err = manager
            .untie_knot(wallet_hex, &untied_knot_hex, "again")
            .unwrap_err();
        assert!(err.contains("already untied"), "got: {err}");
    }

    /// Phase 1.6 — whole-ledger erasure survives restart: the descriptor
    /// stays flagged deleted and no payload blob is resurrected.
    #[test]
    fn erased_ledger_stays_erased_after_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger_db");
        let wallet_hex = "0x000000000000000000000000000000000000c003";

        {
            let manager = make_persistent_manager(&db_path);
            manager.create_ledger(wallet_hex).unwrap();
            manager
                .append_to_ledger(wallet_hex, make_test_interaction(20))
                .unwrap();
            manager
                .erase_ledger(wallet_hex, DeletionReason::GdprArticle17)
                .unwrap();
            assert!(manager
                .store
                .await_all_durable(std::time::Duration::from_secs(10)));
        }

        let manager = make_persistent_manager(&db_path);
        let wallet_bytes = WalletAddress::from_hex(wallet_hex)
            .unwrap()
            .as_bytes()
            .to_vec();
        let desc = manager
            .registry
            .get_descriptor(&wallet_bytes)
            .expect("deleted descriptor must survive restart as an audit record");
        assert!(desc.is_deleted, "is_deleted flag must persist");
        assert!(
            manager.repatriate_ledger(wallet_hex, false).is_err(),
            "repatriating an erased ledger must fail after restart"
        );
        // No payload blob may have been resurrected into the lattice.
        for sid in manager.store.get_chain(&wallet_bytes) {
            assert!(
                manager.lattice.get_string(&StringId::new(sid)).is_none(),
                "erased knot payload must NOT reappear after restart"
            );
        }
    }

    /// Phase 1.6.1 (2026-08-11 P1) — end-to-end lazy-rehydration read path.
    ///
    /// Writes a wallet with a few knots, closes the store, reopens it
    /// LAZILY (metadata only), and confirms that:
    ///   1. the lattice is initially empty for that wallet's knots,
    ///   2. calling `walk_string_with_tombstones` transparently primes
    ///      the chain from disk and returns the full history,
    ///   3. after the walk the lattice is populated (proving the
    ///      on-demand load actually landed).
    ///
    /// This is the regression guard for the OOM crash-loop we fixed
    /// on 2026-08-11: it proves the lazy path doesn't need the
    /// megabytes-of-blobs eager load to answer read queries correctly.
    #[test]
    fn lazy_rehydrate_walk_primes_chain_on_demand() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger_db");
        let wallet_hex = "0x000000000000000000000000000000000000c010";

        let mut appended_ids = Vec::new();
        {
            let manager = make_persistent_manager(&db_path);
            manager.create_ledger(wallet_hex).unwrap();
            for i in 0..5u32 {
                let r = manager
                    .append_to_ledger(wallet_hex, make_test_interaction(i))
                    .unwrap();
                appended_ids.push(r.string_id);
            }
            assert!(manager
                .store
                .await_all_durable(std::time::Duration::from_secs(10)));
        }

        // Reopen lazily — no knot blobs eagerly loaded.
        let manager = make_lazy_persistent_manager(&db_path);
        let wallet_bytes = WalletAddress::from_hex(wallet_hex)
            .unwrap()
            .as_bytes()
            .to_vec();
        let chain_on_disk = manager.store.get_chain(&wallet_bytes);
        assert!(
            !chain_on_disk.is_empty(),
            "the on-disk chain mirror must be non-empty after restart"
        );
        // Every knot payload is absent from the lattice at this point.
        for sid in &chain_on_disk {
            assert!(
                manager.lattice.get_string(&StringId::new(*sid)).is_none(),
                "lazy reopen must NOT preload knot payloads"
            );
        }

        // The read path transparently primes the chain and returns the
        // full history (genesis + 5 appends = 6 entries).
        let (_genesis, entries) = manager.walk_string_with_tombstones(wallet_hex).unwrap();
        assert_eq!(entries.len(), 6, "genesis + 5 appended knots");
        assert!(
            entries.iter().all(|e| !e.is_tombstone()),
            "no tombstones on a clean wallet"
        );

        // After the walk, the lattice is populated (i.e. the on-demand
        // load actually landed).
        for sid in &chain_on_disk {
            assert!(
                manager.lattice.get_string(&StringId::new(*sid)).is_some(),
                "priming must have loaded every knot"
            );
        }

        // And the same call is now cheap: subsequent walks hit the
        // fast-path fully in-memory.
        let (_, entries_again) = manager.walk_string_with_tombstones(wallet_hex).unwrap();
        assert_eq!(entries.len(), entries_again.len());
    }

    /// Phase 1.6.1 (2026-08-11 P1) — repatriate_ledger under lazy
    /// rehydration returns every knot's payload correctly, proving
    /// the priming step also feeds the encrypted read path.
    #[test]
    fn lazy_rehydrate_repatriate_returns_full_ledger() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger_db");
        let wallet_hex = "0x000000000000000000000000000000000000c011";

        {
            let manager = make_persistent_manager(&db_path);
            manager.create_ledger(wallet_hex).unwrap();
            for i in 100..103u32 {
                manager
                    .append_to_ledger(wallet_hex, make_test_interaction(i))
                    .unwrap();
            }
            assert!(manager
                .store
                .await_all_durable(std::time::Duration::from_secs(10)));
        }

        // Reopen lazily and repatriate — must round-trip all 4 knots
        // (genesis + 3 appends) even though none of them were preloaded.
        let manager = make_lazy_persistent_manager(&db_path);
        let rep = manager.repatriate_ledger(wallet_hex, true).unwrap();
        assert_eq!(
            rep.entries.len(),
            4,
            "genesis + 3 appended knots must all repatriate under lazy mode"
        );
    }

    /// Phase 1.6.δ (2026-08-24 lazy-append regression guard).
    ///
    /// Reproduces the "Missing parent string: <head_id>" bug that
    /// blocked every append to the system ledgers (d001..d005) on
    /// new BLUE right after the Phase 10 DNS cutover:
    ///
    ///   1. Write a wallet with a genesis + a few appends, close.
    ///   2. Reopen the store LAZILY (metadata only — knot blobs are
    ///      NOT loaded into the in-memory lattice).
    ///   3. Attempt `append_to_ledger` on that wallet.
    ///
    /// With the pre-fix `append_to_ledger` (no priming), step 3 must
    /// call `add_string(new_string)` with `parent = head_id`,
    /// `add_string` looks up `head_id` in the (empty) lattice, and
    /// returns `RopeError::MissingParent(head_id)` — the caller sees
    /// "Missing parent string: <16 hex>" bubbled through
    /// `append_to_ledger`'s `.map_err(|e| e.to_string())`.
    ///
    /// With the post-fix `append_to_ledger` (calls
    /// `ensure_string_loaded(&head_id)` first), the head is primed
    /// from disk into the lattice, `add_string` finds it, and the
    /// append succeeds. Registry `entry_count` advances by one and
    /// the chain in the lattice extends by one.
    ///
    /// The test also covers the genesis-sentinel edge case
    /// (`head_id == [0u8; 32]` right after `create_ledger`): the
    /// priming call must not fail spuriously because the sentinel is
    /// never on disk and `add_string` already special-cases it.
    #[test]
    fn lazy_rehydrate_append_primes_head_and_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger_db");
        let wallet_hex = "0x000000000000000000000000000000000000d001";

        // Phase A — write, close.
        {
            let manager = make_persistent_manager(&db_path);
            manager.create_ledger(wallet_hex).unwrap();
            for i in 200..203u32 {
                manager
                    .append_to_ledger(wallet_hex, make_test_interaction(i))
                    .unwrap();
            }
            assert!(manager
                .store
                .await_all_durable(std::time::Duration::from_secs(10)));
        }

        // Phase B — reopen LAZILY (metadata only). Prove no knots are
        // preloaded before we attempt the append.
        let manager = make_lazy_persistent_manager(&db_path);
        let wallet_bytes = WalletAddress::from_hex(wallet_hex)
            .unwrap()
            .as_bytes()
            .to_vec();
        let chain_on_disk = manager.store.get_chain(&wallet_bytes);
        assert_eq!(
            chain_on_disk.len(),
            4,
            "on-disk chain must have genesis + 3 appends"
        );
        for sid in &chain_on_disk {
            assert!(
                manager.lattice.get_string(&StringId::new(*sid)).is_none(),
                "lazy reopen must NOT preload knot payloads (regression \
                 guard for 2026-08-11 OOM fix)"
            );
        }
        let pre_desc = manager
            .registry
            .get_descriptor(&wallet_bytes)
            .expect("descriptor must survive lazy reopen");
        let pre_head = pre_desc.head_string_id;
        let pre_count = pre_desc.entry_count;
        assert_eq!(
            pre_count, 4,
            "descriptor must remember the 4 knots written before reopen"
        );

        // Phase C — attempt append_to_ledger. With the pre-fix this
        // call fails with `Missing parent string: <16 hex of pre_head>`.
        // With the post-fix it must succeed AND leave the head knot
        // primed in the lattice.
        let resp = manager
            .append_to_ledger(wallet_hex, make_test_interaction(203))
            .expect(
                "append after lazy reopen MUST succeed — if you see \
                 'Missing parent string' here, the head-priming call \
                 in append_to_ledger was removed and the 2026-08-24 \
                 d001..d005 governance-append bug has been reintroduced",
            );

        // Post-check: the newly-appended knot must extend the chain,
        // and the OLD head that used to be disk-only is now in the
        // lattice (proof that the priming step landed).
        let post_desc = manager
            .registry
            .get_descriptor(&wallet_bytes)
            .expect("descriptor must remain");
        assert_eq!(
            post_desc.entry_count,
            pre_count + 1,
            "entry_count must advance by exactly one after the append"
        );
        assert_ne!(
            post_desc.head_string_id, pre_head,
            "head must move to the newly-appended knot"
        );
        assert!(
            manager.lattice.get_string(&pre_head).is_some(),
            "the head that WAS disk-only before the append must now be \
             in the lattice — this is the direct behavioural proof that \
             ensure_string_loaded(&head_id) ran inside append_to_ledger"
        );
        // The knot we just appended must also be in the lattice.
        let new_head_id = StringId::from_hex(&resp.string_id).unwrap();
        assert!(
            manager.lattice.get_string(&new_head_id).is_some(),
            "the newly-appended knot must be in the lattice"
        );

        // Second append on the same wallet must also succeed (fast
        // path: new head is now in the lattice from the previous
        // append, so priming is a no-op).
        manager
            .append_to_ledger(wallet_hex, make_test_interaction(204))
            .expect("second append after lazy reopen must also succeed");
    }

    /// Phase 1.6.δ first-append-after-create-then-lazy-reopen guard.
    ///
    /// Covers the smallest possible reproducer of the d001..d005 bug:
    /// a wallet that has ONLY the genesis knot on disk (created by
    /// `create_ledger` and then the store closed). Under lazy reopen
    /// the genesis knot's payload is NOT in the lattice; the priming
    /// step in `append_to_ledger` must load it before `add_string`
    /// checks the parent. Without priming this first-append fails with
    /// "Missing parent string: <genesis knot id>".
    #[test]
    fn lazy_rehydrate_append_on_genesis_only_wallet_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("ledger_db");
        let wallet_hex = "0x000000000000000000000000000000000000d0aa";

        // Phase A — create the ledger (writes only the genesis knot),
        // close before appending anything.
        let genesis_head: StringId = {
            let manager = make_persistent_manager(&db_path);
            manager.create_ledger(wallet_hex).unwrap();
            assert!(manager
                .store
                .await_all_durable(std::time::Duration::from_secs(10)));
            let wallet_bytes = WalletAddress::from_hex(wallet_hex)
                .unwrap()
                .as_bytes()
                .to_vec();
            manager
                .registry
                .get_descriptor(&wallet_bytes)
                .expect("descriptor")
                .head_string_id
        };

        // Phase B — reopen lazily. head_string_id is a real 32-byte
        // hash (the genesis knot), NOT the all-zero sentinel, and
        // it is NOT in the lattice.
        let manager = make_lazy_persistent_manager(&db_path);
        assert!(
            !genesis_head.as_bytes().iter().all(|&b| b == 0),
            "post-create head must be a real genesis knot id, not the \
             all-zero sentinel — the sentinel is only used internally \
             during genesis construction, never as a persisted head"
        );
        assert!(
            manager.lattice.get_string(&genesis_head).is_none(),
            "genesis knot must be disk-only after lazy reopen"
        );

        // Phase C — first append after lazy reopen must succeed. This
        // is the exact production reproducer that hit d001..d005 on
        // new BLUE 2026-08-24: a wallet whose head is on disk and not
        // in the lattice at append time.
        manager
            .append_to_ledger(wallet_hex, make_test_interaction(1))
            .expect(
                "first append after create_ledger + lazy reopen must \
                 succeed — this is the d001..d005 production reproducer",
            );

        // After the append, the genesis knot is now in the lattice
        // (primed) and the new knot is chained to it.
        assert!(
            manager.lattice.get_string(&genesis_head).is_some(),
            "genesis knot must be primed into the lattice by the append"
        );
        let wallet_bytes = WalletAddress::from_hex(wallet_hex)
            .unwrap()
            .as_bytes()
            .to_vec();
        let post = manager
            .registry
            .get_descriptor(&wallet_bytes)
            .expect("descriptor remains");
        assert_eq!(post.entry_count, 2, "genesis (1) + first append (1)");
    }
}
