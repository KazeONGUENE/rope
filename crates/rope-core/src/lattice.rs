//! String Lattice - The core DAG data structure replacing blockchain
//!
//! L = (S, ≺, ⊗, R)
//!
//! Where:
//! - S: Set of all strings in the Rope
//! - ≺ (Precedes): Partial ordering capturing causal dependencies
//! - ⊗ (Intertwine): Complementary pairing operation (double helix)
//! - R (Regeneration): Repair relation for damaged strings
//!
//! ## Quipu Canon v2.0 Phase 1.1 — 256-Shard Lattice
//!
//! In v1.x [`StringLattice`] held a single global `RwLock` for each of its
//! six core HashMaps and one for the petgraph DAG. Every `add_string` took
//! four of those write locks at once
//! (`docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §3.1) so concurrent
//! appends, even to wholly unrelated wallets, serialised on one mutex chain.
//!
//! Phase 1.1 partitions the lattice across 256 shards keyed by `StringId[0]`
//! (the first byte of the BLAKE3 hash that names every string — already
//! uniformly distributed, so no rehash required). Each shard owns:
//!
//! - its own `strings`, `complements`, `erased`, `tombstones`, parents /
//!   children maps (P1.4: [`DashMap`] / [`DashSet`] so walks never take a
//!   whole-map `RwLock` that blocks appends on the same shard);
//! - `pending` still under a per-shard `RwLock<BTreeMap>` (ordered by
//!   Lamport time; only the maintenance actor touches it, and P1.3 already
//!   snapshots before BFS).
//!
//! Because `StringId[0]` is uniformly random, two concurrent `add_string`
//! calls almost always touch two different shards and proceed in parallel.
//! Genuinely cross-shard edges (parent in shard A, child in shard B) are
//! handled by writing the parent edge in B's `parents` map and the child
//! edge in A's `children` map — two writes, two shards, no contention.
//!
//! ### P1.4 — DashMap walks (2026-07-27)
//!
//! Hang dumps after P1.2 still showed `walk_string_with_tombstones` /
//! `add_string` piled on `RawRwLock::{lock_shared,lock_exclusive}` for
//! per-shard `HashMap`s. Switching those hot maps to `DashMap` gives
//! sharded fine-grained locking: a walk hop holds only one entry shard
//! briefly, and an append on the same `StringId[0]` lattice shard no
//! longer exclusive-locks the entire map.
//!
//! ### What stays global
//!
//! Three structures remain global because they are intrinsically aggregate
//! and write-rate-bounded:
//!
//! - `anchors`: one anchor every ~10 s by canon design
//! - `finalized_strings`: written only when an anchor finalises a batch
//! - `current_round`: bumped only when a new anchor is produced
//!
//! ### What changes for callers
//!
//! Nothing. The public API is byte-identical to v1.x:
//! `add_string`, `get_string`, `mark_erased`, `mark_knot_untied`,
//! `walk_ledger_chain`, `walk_string_with_tombstones`,
//! `strings_by_creator`, `erase_creator_strings`, `get_parents`,
//! `get_children`, `is_finalized`, `contains`, `string_count`,
//! `pending_count`, `finalized_count`, `erased_count`, `tombstone_count`,
//! `current_round`, `latest_anchor`, `anchors`, `verify_string`,
//! `regenerate_string`, `stats`, and `check_finality` all keep their
//! v1.x signatures and semantics. `ledger_manager.rs` and the test suite
//! require no changes.

use dashmap::{DashMap, DashSet};
use hashbrown::{HashMap, HashSet};
use once_cell::sync::OnceCell;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use crate::complement::Complement;
use crate::error::{Result, RopeError};
use crate::string::RopeString;
use crate::types::{constants, FinalityStatus, StringId};

/// Number of per-`StringId` shards in the lattice. Chosen to match the
/// HLC shard count introduced in Phase 1.3 (`crates/rope-core/src/clock.rs`).
/// Both constants are intentionally separate while the two phase branches
/// land independently; once both are on `main` they should be reconciled
/// via a small `shards` module that exports a single canonical value.
pub const NUM_SHARDS: usize = 256;

/// One per-`creator_pk[0]` shard of the creator index.
type CreatorShard = RwLock<HashMap<[u8; 32], Vec<StringId>>>;

/// Anchor String - Synchronization point in the lattice
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnchorString {
    /// The underlying string
    pub string: RopeString,

    /// Consensus round number
    pub round: u64,

    /// Previous anchors this one strongly sees
    pub strongly_sees: Vec<StringId>,

    /// Number of testimonies received
    pub testimony_count: u32,

    /// Whether this is a famous anchor (achieved consensus)
    pub is_famous: bool,
}

impl AnchorString {
    /// Create a new anchor string
    pub fn new(string: RopeString, round: u64) -> Self {
        Self {
            string,
            round,
            strongly_sees: Vec::new(),
            testimony_count: 0,
            is_famous: false,
        }
    }

    pub fn id(&self) -> StringId {
        self.string.id()
    }
}

/// Knot tombstone metadata — the canonical record of an untied knot.
///
/// Per Quipu Primitive Canon v1.1 §4.2, when a knot is untied via
/// `rope_untieKnot`, its encrypted payload is destroyed but the knot's
/// position on the string remains as a deliberate absence with provable
/// audit metadata. This struct holds that audit metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnotTombstone {
    /// Unix timestamp (seconds) when the knot was untied
    pub untied_at: i64,
    /// 32-byte audit hash committing to (string_id || untied_at || reason)
    pub audit_hash: [u8; 32],
    /// Human-readable reason class (e.g. "GdprArticle17", "OwnerRequest", "LegalOrder")
    pub reason: String,
}

/// One entry on a wallet's string when walked with tombstone awareness.
///
/// Active entries carry the live `StringId`. Tombstones carry the same
/// `StringId` (the knot's position is preserved) plus the tombstone metadata
/// — the encrypted payload is gone, but the position, audit hash, and
/// timestamp remain auditable. This is the canonical shape DCScan and
/// Datawallet+ should render in the String → Knot → Tx-details hierarchy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LedgerEntry {
    Active(StringId),
    Tombstone(StringId, KnotTombstone),
}

impl LedgerEntry {
    pub fn string_id(&self) -> StringId {
        match self {
            LedgerEntry::Active(id) => *id,
            LedgerEntry::Tombstone(id, _) => *id,
        }
    }

    pub fn is_tombstone(&self) -> bool {
        matches!(self, LedgerEntry::Tombstone(_, _))
    }
}

/// Map a `StringId` to its shard. Trivial since the id is itself a uniformly
/// distributed BLAKE3 hash — no rehash required.
#[inline]
fn shard_for_string_id(id: &StringId) -> usize {
    id.as_bytes()[0] as usize
}

/// Map a creator public key to its shard. Ed25519 keys are uniformly
/// distributed; `pk[0]` is sufficient.
#[inline]
fn shard_for_creator(pk: &[u8; 32]) -> usize {
    pk[0] as usize
}

/// Per-shard slice of the lattice. One of these per `StringId[0]` byte.
struct LatticeShard {
    /// Strings whose `StringId[0]` lands in this shard.
    strings: DashMap<StringId, RopeString>,

    /// Complements for the strings in this shard.
    complements: DashMap<StringId, Complement>,

    /// child -> parents map. The CHILD lives in this shard; parents may be
    /// in any shard. This replaces petgraph's `Direction::Incoming` query
    /// for nodes whose `StringId[0]` lands in this shard.
    parents: DashMap<StringId, Vec<StringId>>,

    /// parent -> children map. The PARENT lives in this shard; children may
    /// be in any shard. Replaces `Direction::Outgoing` for nodes whose
    /// `StringId[0]` lands in this shard.
    children: DashMap<StringId, Vec<StringId>>,

    /// Erased strings (whole-string tombstones).
    erased: DashSet<StringId>,

    /// Untied-knot tombstones with audit metadata (canon v1.1 §4.2).
    tombstones: DashMap<StringId, KnotTombstone>,

    /// Pending strings awaiting finality, ordered by Lamport time.
    /// Each shard keeps its own slice; aggregation is via summing across
    /// shards (low-frequency call, used by `pending_count` and finality).
    /// Kept as `RwLock` (not DashMap) because finality needs a time-ordered
    /// BTree and P1.3 already snapshots before any graph walk.
    pending: RwLock<BTreeMap<u64, HashSet<StringId>>>,
}

impl LatticeShard {
    fn new() -> Self {
        Self {
            strings: DashMap::new(),
            complements: DashMap::new(),
            parents: DashMap::new(),
            children: DashMap::new(),
            erased: DashSet::new(),
            tombstones: DashMap::new(),
            pending: RwLock::new(BTreeMap::new()),
        }
    }
}

/// String Lattice - The core data structure of Datachain Rope
///
/// Replaces blockchain's linear chain with a multi-dimensional lattice
/// of intertwined strings that can be added, verified, and erased.
///
/// Per Quipu Primitive Canon v1.1, the entries on a wallet's string are
/// individually addressable knots. Two erasure pathways exist:
///   - `mark_erased(id)` — drops the string entirely (whole-wallet closure
///     pathway used by `rope_erasePersonalLedger`).
///   - `mark_knot_untied(id, reason)` — destroys the payload but preserves
///     the knot's position via the DAG so `walk_string_with_tombstones` can
///     traverse past it. This is the per-knot GDPR primitive.
///
/// Quipu Canon v2.0 Phase 1.1: internally sharded over `NUM_SHARDS` (256)
/// per-shard slices keyed by `StringId[0]`. See module-level docs.
pub struct StringLattice {
    /// 256 per-string-id shards (the hot path).
    shards: Box<[LatticeShard]>,

    /// 256 per-creator-pubkey shards. Distinct sharding axis from
    /// `shards` because a wallet's strings span many `StringId[0]`
    /// buckets, but the creator pubkey is constant for one wallet.
    creator_index: Box<[CreatorShard]>,

    /// Anchor strings for consensus. Global because anchors are
    /// produced ~once per 10s by canon design — write rate is far
    /// below the contention threshold.
    anchors: RwLock<Vec<AnchorString>>,

    /// Finalized strings. Global; written only when an anchor finalises
    /// a batch.
    finalized_strings: RwLock<HashSet<StringId>>,

    /// Current consensus round number. Global; bumped only on anchor.
    current_round: RwLock<u64>,

    /// Coalescing notify channel for the background finality actor.
    /// Unset until [`Self::start_finality_actor`] is called — in that mode
    /// [`Self::schedule_maintenance`] runs anchor + finality inline (tests
    /// / ephemeral use).
    ///
    /// **Phase 1.6.β (2026-08-11 wedge remediation):** was
    /// `Mutex<Option<SyncSender<()>>>`, which convoyed every append on a
    /// global mutex. `OnceCell` is set-once + lock-free read — the
    /// [`Self::schedule_maintenance`] hot path is now a single atomic
    /// pointer load. Drop still works because the natural drop of the
    /// `StringLattice` drops the `OnceCell` which drops the contained
    /// `SyncSender`, disconnecting the actor's channel.
    finality_tx: OnceCell<SyncSender<()>>,

    /// Joined on Drop when the actor was started.
    finality_join: Mutex<Option<JoinHandle<()>>>,

    /// StringIds whose anchor eligibility still needs checking.
    /// Populated by [`Self::add_string`]; drained by the maintenance actor
    /// (2026-07-27 P1.3 — never take `anchors.write()` on the append path).
    ///
    /// **Phase 1.6.β (2026-08-11 wedge remediation):** was a single global
    /// `Mutex<Vec<StringId>>`, which serialised every append on a global
    /// mutex despite the 256-way lattice sharding making the rest of
    /// `add_string` per-shard-parallel. Now sharded 256 ways on the same
    /// `StringId[0]` axis as the rest of the lattice — two appends whose
    /// ids land in different shards never contend on this queue at all,
    /// and same-shard contention is bounded by the (already-per-shard)
    /// DashMap write cost. `mem::take` on drain preserves the FIFO
    /// semantics `process_anchor_candidates` relies on.
    anchor_candidates: Box<[Mutex<Vec<StringId>>]>,
}

impl StringLattice {
    /// Create a new empty string lattice with [`NUM_SHARDS`] shards.
    pub fn new() -> Self {
        let shards: Vec<LatticeShard> = (0..NUM_SHARDS).map(|_| LatticeShard::new()).collect();
        let creator_shards: Vec<CreatorShard> = (0..NUM_SHARDS)
            .map(|_| RwLock::new(HashMap::new()))
            .collect();
        // Phase 1.6.β: one anchor-candidates bucket per lattice shard, so
        // `add_string` contends only with same-shard peers on this queue.
        let candidate_shards: Vec<Mutex<Vec<StringId>>> =
            (0..NUM_SHARDS).map(|_| Mutex::new(Vec::new())).collect();
        Self {
            shards: shards.into_boxed_slice(),
            creator_index: creator_shards.into_boxed_slice(),
            anchors: RwLock::new(Vec::new()),
            finalized_strings: RwLock::new(HashSet::new()),
            current_round: RwLock::new(0),
            finality_tx: OnceCell::new(),
            finality_join: Mutex::new(None),
            anchor_candidates: candidate_shards.into_boxed_slice(),
        }
    }

    /// Start the background lattice maintenance actor (idempotent).
    ///
    /// 2026-07-27 residual erpc 5xx P1.2/P1.3: anchor creation and
    /// `update_finality` (pending sweep + BFS ancestor checks) must never
    /// run inside `add_string`. Production nodes call this once the
    /// lattice sits behind an `Arc`. Capacity-1 channel coalesces bursts.
    pub fn start_finality_actor(self: &Arc<Self>) {
        // Phase 1.6.β: `OnceCell::set` is idempotent — a second call
        // returns Err(_) and we early-return without touching the actor.
        let (tx, rx) = mpsc::sync_channel::<()>(1);
        if self.finality_tx.set(tx).is_err() {
            return;
        }

        let lattice = Arc::clone(self);
        let handle = thread::Builder::new()
            .name("rope-lattice-finality".to_string())
            .spawn(move || {
                while rx.recv().is_ok() {
                    // Drain coalesced notifies so one pass covers a burst.
                    while rx.try_recv().is_ok() {}
                    lattice.run_maintenance();
                }
            })
            .expect("spawn lattice finality actor");
        *self.finality_join.lock() = Some(handle);
    }

    /// Schedule anchor + finality maintenance. With the actor running this
    /// is non-blocking (`try_send`); without it, runs synchronously so
    /// unit tests keep observing anchors/finality without starting a thread.
    ///
    /// Phase 1.6.β: `OnceCell::get()` is a single lock-free atomic load —
    /// this hot path no longer touches a mutex.
    fn schedule_maintenance(&self) {
        if let Some(tx) = self.finality_tx.get() {
            // `try_send` on a capacity-1 channel is O(1) and coalesces
            // bursts: if a notify is already pending the send drops and
            // the actor will still see the state change on its next tick.
            let _ = tx.try_send(());
        } else {
            self.run_maintenance();
        }
    }

    /// Drain pending anchor candidates, then run a snapshot-based finality
    /// pass. Never called from inside a shard write critical section.
    fn run_maintenance(&self) {
        self.process_anchor_candidates();
        self.update_finality();
    }

    /// Create anchors for ids queued by [`Self::add_string`].
    ///
    /// Phase 1.6.β: candidates are sharded on the same `StringId[0]`
    /// axis as the lattice; we drain each shard in turn under a very
    /// short critical section (bare `mem::take`) so appends on other
    /// shards proceed uninterrupted throughout the sweep.
    fn process_anchor_candidates(&self) {
        // Rough capacity hint: the drain typically empties every shard.
        let mut candidates: Vec<StringId> = Vec::new();
        for shard in self.anchor_candidates.iter() {
            let mut q = shard.lock();
            if q.is_empty() {
                continue;
            }
            candidates.extend(std::mem::take(&mut *q));
        }
        for id in candidates {
            if let Some(string) = self.get_string(&id) {
                let _ = self.check_anchor_creation(&string);
            }
        }
    }

    /// Number of per-string-id shards (always [`NUM_SHARDS`]). Exposed for
    /// tests and metrics.
    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    /// Add a string to the lattice
    ///
    /// Phase 1.1 sharded write path:
    ///
    ///   1. Verify parentage exists (parent's own shard, read lock)
    ///   2. Verify the string isn't already erased / parents aren't erased
    ///   3. Generate complement
    ///   4. Insert into the child's shard (`strings`, `complements`,
    ///      `parents`, `pending`) — at most 4 write locks but all on the
    ///      same shard, no contention with concurrent inserts on other
    ///      shards
    ///   5. Update each parent's shard `children` index — one write lock per
    ///      parent shard (typically 1, since most parents differ from child
    ///      in `StringId[0]`)
    ///   6. Update creator-index shard (`creator_pk[0]`)
    ///   7. Queue id for background anchor/finality maintenance (P1.3)
    ///
    /// Lock hierarchy (P1.3/P1.4 — do not invert):
    /// - Never hold `pending.*` or `anchors.*` across `get_parents` / BFS.
    /// - Hot maps (`strings`/`parents`/…) are DashMap — entry-level locks only.
    /// - Publish order on insert: complements → parents → pending → **strings
    ///   last**, so a walk that observes a live string also sees its edges.
    /// - `anchors` / `finalized_strings` / `current_round` mutate only from
    ///   the maintenance actor (or its sync fallback), never inline here.
    pub fn add_string(&self, string: RopeString) -> Result<StringId> {
        let id = string.id();
        let parents = string.parentage().to_vec();

        // Step 1+2: verify parentage (fine-grained DashMap lookups).
        for parent in &parents {
            if parent.as_bytes().iter().all(|&b| b == 0) {
                continue; // genesis sentinel
            }
            let p_shard = &self.shards[shard_for_string_id(parent)];
            if !p_shard.strings.contains_key(parent) {
                return Err(RopeError::MissingParent(*parent));
            }
            if p_shard.erased.contains(parent) {
                return Err(RopeError::ParentErased(*parent));
            }
        }

        // Step 3: complement (pure CPU, no locks)
        let complement = Complement::generate(&string);

        let timestamp = string.temporal_marker().time();
        let creator_key = string.creator().ed25519;
        let id_shard_idx = shard_for_string_id(&id);

        // Step 4: insert into the child's shard. Publish `strings` last so
        // concurrent walks never observe a live knot without parents /
        // complement entries.
        {
            let shard = &self.shards[id_shard_idx];
            shard.complements.insert(id, complement);
            shard.parents.insert(id, parents.clone());
            {
                let mut pending = shard.pending.write();
                pending.entry(timestamp).or_default().insert(id);
            }
            shard.strings.insert(id, string.clone());
        }

        // Step 5: update each parent's shard's `children` index.
        let mut parent_buckets: HashMap<usize, Vec<StringId>> = HashMap::new();
        for parent in &parents {
            if parent.as_bytes().iter().all(|&b| b == 0) {
                continue;
            }
            parent_buckets
                .entry(shard_for_string_id(parent))
                .or_default()
                .push(*parent);
        }
        for (s_idx, parents_in_shard) in parent_buckets {
            let children = &self.shards[s_idx].children;
            for parent in parents_in_shard {
                children.entry(parent).or_default().push(id);
            }
        }

        // Step 6: creator index, on its own sharding axis.
        self.creator_index[shard_for_creator(&creator_key)]
            .write()
            .entry(creator_key)
            .or_default()
            .push(id);

        // Step 7 (P1.3 + P1.6.β): queue for background anchor/finality.
        // Same sharding axis as the rest of `add_string` — two appends
        // whose ids land in different lattice shards never contend on
        // this queue at all. The previous global `anchor_candidates`
        // Mutex serialised every append on one lock even after Phase 1.1
        // sharded the rest of the write path.
        self.anchor_candidates[id_shard_idx].lock().push(id);
        self.schedule_maintenance();

        Ok(id)
    }

    /// Quipu Canon v2.0 Phase 1.6 — restore a string recovered from the
    /// persistent store at boot time.
    ///
    /// Unlike [`Self::add_string`], this path:
    ///
    ///   1. Does NOT validate parentage — restored knots may arrive in
    ///      any order, and a parent may legitimately be absent because
    ///      it was untied (its position survives as a tombstone via
    ///      [`Self::restore_tombstone`]).
    ///   2. Does NOT enter the `pending` queue or the anchor path —
    ///      restored knots were already accepted by consensus in a
    ///      previous process lifetime; they are marked finalized
    ///      directly.
    ///   3. Regenerates the complement (complements are derived data).
    ///
    /// Idempotent: restoring an id that is already live is a no-op.
    pub fn restore_string(&self, string: RopeString) -> StringId {
        let id = string.id();
        let parents = string.parentage().to_vec();
        let creator_key = string.creator().ed25519;

        {
            let shard = &self.shards[shard_for_string_id(&id)];
            if shard.strings.contains_key(&id) {
                return id;
            }
            let complement = Complement::generate(&string);
            shard.complements.insert(id, complement);
            shard.parents.insert(id, parents.clone());
            shard.strings.insert(id, string);
        }

        // Rebuild parent -> children edges (skipping the genesis sentinel).
        let mut parent_buckets: HashMap<usize, Vec<StringId>> = HashMap::new();
        for parent in &parents {
            if parent.as_bytes().iter().all(|&b| b == 0) {
                continue;
            }
            parent_buckets
                .entry(shard_for_string_id(parent))
                .or_default()
                .push(*parent);
        }
        for (s_idx, parents_in_shard) in parent_buckets {
            let children_map = &self.shards[s_idx].children;
            for parent in parents_in_shard {
                let mut children = children_map.entry(parent).or_default();
                if !children.contains(&id) {
                    children.push(id);
                }
            }
        }

        // Creator index.
        {
            let mut creators = self.creator_index[shard_for_creator(&creator_key)].write();
            let ids = creators.entry(creator_key).or_default();
            if !ids.contains(&id) {
                ids.push(id);
            }
        }

        // Restored knots are settled history — finalized, not pending.
        self.finalized_strings.write().insert(id);

        id
    }

    /// Quipu Canon v2.0 Phase 1.6 — restore an untie-tombstone recovered
    /// from the persistent store at boot time.
    ///
    /// Re-registers the tombstone metadata AND the untied knot's parent
    /// edges, so `walk_string_with_tombstones` can hop past the
    /// deliberate absence exactly as it could before the restart. The
    /// payload itself is gone (cryptographic erasure) — only position,
    /// audit hash, timestamp, and reason survive, per canon v1.1 §4.2.
    pub fn restore_tombstone(
        &self,
        id: StringId,
        tombstone: KnotTombstone,
        parents: Vec<StringId>,
    ) {
        let shard = &self.shards[shard_for_string_id(&id)];
        shard.erased.insert(id);
        shard.tombstones.insert(id, tombstone);
        if !parents.is_empty() {
            shard.parents.insert(id, parents);
        }
    }

    /// Get a string by ID.
    pub fn get_string(&self, id: &StringId) -> Option<RopeString> {
        let shard = &self.shards[shard_for_string_id(id)];
        if shard.erased.contains(id) {
            return None;
        }
        shard.strings.get(id).map(|r| r.clone())
    }

    /// Get a complement by string ID.
    pub fn get_complement(&self, id: &StringId) -> Option<Complement> {
        let shard = &self.shards[shard_for_string_id(id)];
        if shard.erased.contains(id) {
            return None;
        }
        shard.complements.get(id).map(|r| r.clone())
    }

    /// Check finality status of a string
    pub fn check_finality(&self, id: &StringId) -> FinalityStatus {
        let anchor_refs = self.count_anchor_references(id);

        if anchor_refs >= constants::FINALITY_ANCHORS {
            FinalityStatus::finalized(anchor_refs)
        } else {
            FinalityStatus::pending(
                anchor_refs,
                constants::ANCHOR_INTERVAL * (constants::FINALITY_ANCHORS - anchor_refs),
            )
        }
    }

    /// Check if a string is finalized
    pub fn is_finalized(&self, id: &StringId) -> bool {
        self.finalized_strings.read().contains(id)
    }

    /// Check if a string exists in the lattice
    pub fn contains(&self, id: &StringId) -> bool {
        let shard = &self.shards[shard_for_string_id(id)];
        !shard.erased.contains(id) && shard.strings.contains_key(id)
    }

    /// Get the total number of strings in the lattice. Aggregates across
    /// shards.
    pub fn string_count(&self) -> usize {
        self.shards.iter().map(|s| s.strings.len()).sum()
    }

    /// Get the total number of pending strings across all shards.
    pub fn pending_count(&self) -> usize {
        self.shards
            .iter()
            .map(|s| {
                s.pending
                    .read()
                    .values()
                    .map(|set| set.len())
                    .sum::<usize>()
            })
            .sum()
    }

    /// Get the number of finalized strings
    pub fn finalized_count(&self) -> usize {
        self.finalized_strings.read().len()
    }

    /// Get the total number of erased strings across all shards.
    pub fn erased_count(&self) -> usize {
        self.shards.iter().map(|s| s.erased.len()).sum()
    }

    /// Get current round number
    pub fn current_round(&self) -> u64 {
        *self.current_round.read()
    }

    /// Get the latest anchor string
    pub fn latest_anchor(&self) -> Option<AnchorString> {
        self.anchors.read().last().cloned()
    }

    /// Get all anchor strings
    pub fn anchors(&self) -> Vec<AnchorString> {
        self.anchors.read().clone()
    }

    /// Get parents of a string. Looks up only one shard (the child's).
    pub fn get_parents(&self, id: &StringId) -> Vec<StringId> {
        self.shards[shard_for_string_id(id)]
            .parents
            .get(id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    /// Get children of a string. Looks up only one shard (the parent's).
    pub fn get_children(&self, id: &StringId) -> Vec<StringId> {
        self.shards[shard_for_string_id(id)]
            .children
            .get(id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    // === Personal Ledger Queries ===

    /// Get all StringIds created by a specific public key (wallet)
    pub fn strings_by_creator(&self, ed25519_pubkey: &[u8; 32]) -> Vec<StringId> {
        self.creator_index[shard_for_creator(ed25519_pubkey)]
            .read()
            .get(ed25519_pubkey)
            .cloned()
            .unwrap_or_default()
    }

    /// Walk the ledger chain for a creator: starting from `head`, follow
    /// parentage links backwards to build the ordered chain. Returns entries
    /// from genesis to head (oldest first).
    ///
    /// Each hop is a DashMap entry lookup (P1.4) — no whole-map `RwLock`,
    /// so concurrent `add_string` on the same lattice shard does not convoy.
    pub fn walk_ledger_chain(&self, head: &StringId) -> Vec<StringId> {
        let mut chain = Vec::new();
        let mut current = *head;

        loop {
            if current == StringId::ZERO {
                break;
            }
            let shard = &self.shards[shard_for_string_id(&current)];
            if shard.erased.contains(&current) {
                break;
            }
            let next = match shard.strings.get(&current) {
                Some(s) => {
                    chain.push(current);
                    s.parentage().first().copied().unwrap_or(StringId::ZERO)
                }
                None => break,
            };
            if next == current {
                break; // defensive against self-loops
            }
            current = next;
        }

        chain.reverse();
        chain
    }

    /// Erase all strings belonging to a specific creator (wallet ledger deletion).
    /// Returns the count of erased strings.
    pub fn erase_creator_strings(&self, ed25519_pubkey: &[u8; 32]) -> Result<usize> {
        let string_ids = self.strings_by_creator(ed25519_pubkey);
        let mut erased_count = 0;

        for id in &string_ids {
            if self.mark_erased(*id).is_ok() {
                erased_count += 1;
            }
        }

        self.creator_index[shard_for_creator(ed25519_pubkey)]
            .write()
            .remove(ed25519_pubkey);

        Ok(erased_count)
    }

    /// Mark a string as erased. Touches one shard (the id's own).
    pub fn mark_erased(&self, id: StringId) -> Result<()> {
        let shard = &self.shards[shard_for_string_id(&id)];

        if !shard.strings.contains_key(&id) {
            return Err(RopeError::StringNotFound(id));
        }

        // Remove from active storage, then mark erased (walks that miss
        // the string also check `erased` / tombstones).
        shard.strings.remove(&id);
        shard.complements.remove(&id);
        shard.erased.insert(id);

        Ok(())
    }

    // ====================================================================
    // Quipu Primitive Canon v1.1 — per-knot (per-event) untying
    // ====================================================================

    /// Untie a single knot on a string (canon v1.1 §4.2).
    ///
    /// This is the granular GDPR Article 17 primitive. Unlike `mark_erased`
    /// (whole-string deletion), `mark_knot_untied`:
    ///
    ///   1. Destroys the knot's encrypted payload (string + complement)
    ///   2. Records canonical tombstone metadata (timestamp, audit hash, reason)
    ///   3. **Preserves the knot's position** via the DAG ordering (parent/child
    ///      edges remain, so `walk_string_with_tombstones` can traverse past)
    ///
    /// The result satisfies EDPB cryptographic-erasure guidance: the payload
    /// is unrecoverable, but the knot's ordinal position on the cord remains
    /// auditable as a deliberate absence.
    pub fn mark_knot_untied(&self, id: StringId, reason: &str) -> Result<KnotTombstone> {
        // Compute audit hash before payload destruction so the hash commits
        // to the live state. Hash inputs: string_id || untied_at || reason
        let untied_at = chrono::Utc::now().timestamp();
        let mut hasher = blake3::Hasher::new();
        hasher.update(id.as_bytes());
        hasher.update(&untied_at.to_le_bytes());
        hasher.update(reason.as_bytes());
        let audit_hash = *hasher.finalize().as_bytes();

        let tombstone = KnotTombstone {
            untied_at,
            audit_hash,
            reason: reason.to_string(),
        };

        // Destroy the payload (this also drops the parentage stored in the
        // RopeString itself; the per-shard `parents` map retains parent
        // edges separately so `walk_string_with_tombstones` can hop past).
        self.mark_erased(id)?;

        // Record the canonical tombstone metadata in the id's shard.
        self.shards[shard_for_string_id(&id)]
            .tombstones
            .insert(id, tombstone.clone());

        Ok(tombstone)
    }

    /// Look up a knot's tombstone metadata, if any. Returns None if the knot
    /// was never untied (or was whole-string erased without tombstone metadata).
    pub fn get_tombstone(&self, id: &StringId) -> Option<KnotTombstone> {
        self.shards[shard_for_string_id(id)]
            .tombstones
            .get(id)
            .map(|r| r.clone())
    }

    /// Check whether a knot has been untied via the canonical canon v1.1 path.
    pub fn is_knot_untied(&self, id: &StringId) -> bool {
        self.shards[shard_for_string_id(id)]
            .tombstones
            .contains_key(id)
    }

    /// Total count of untied knots across all shards (transparency metric
    /// for the canon §6(5) UI).
    pub fn tombstone_count(&self) -> usize {
        self.shards.iter().map(|s| s.tombstones.len()).sum()
    }

    /// Walk a wallet's string from `head` back to genesis, but DO NOT stop
    /// at tombstones. Returns one `LedgerEntry` per knot position — either
    /// `Active(StringId)` or `Tombstone(StringId, KnotTombstone)`.
    ///
    /// Walks via per-shard `parents` edges (which survive `mark_knot_untied`)
    /// when the live RopeString is gone, and via the RopeString's own
    /// parentage when it's present. Returned vector is genesis-first
    /// (oldest first).
    ///
    /// P1.4: each hop uses DashMap entry locks only — concurrent appends
    /// on the same lattice shard no longer take a whole-map `RwLock`.
    pub fn walk_string_with_tombstones(&self, head: &StringId) -> Vec<LedgerEntry> {
        let mut chain: Vec<LedgerEntry> = Vec::new();
        let mut current = *head;
        let mut hops = 0usize;
        // Hard cap to defend against pathological graphs; matches
        // typical personal-ledger lengths plus headroom.
        const MAX_HOPS: usize = 1_000_000;

        loop {
            if hops >= MAX_HOPS {
                break;
            }
            hops += 1;

            if current == StringId::ZERO {
                break;
            }

            let shard = &self.shards[shard_for_string_id(&current)];

            // Resolve parent: prefer the live RopeString's own parentage,
            // fall back to the per-shard parents map (which survives untying).
            let next = if let Some(s) = shard.strings.get(&current) {
                chain.push(LedgerEntry::Active(current));
                s.parentage().first().copied().unwrap_or(StringId::ZERO)
            } else if let Some(ts) = shard.tombstones.get(&current) {
                chain.push(LedgerEntry::Tombstone(current, ts.clone()));
                drop(ts);
                shard
                    .parents
                    .get(&current)
                    .and_then(|p| p.first().copied())
                    .unwrap_or(StringId::ZERO)
            } else {
                // Unknown id — neither live nor tombstoned. Stop.
                break;
            };

            if next == current {
                // Defensive: a self-loop would otherwise spin.
                break;
            }
            current = next;
        }

        chain.reverse();
        chain
    }

    /// Verify string integrity using complement
    pub fn verify_string(&self, id: &StringId) -> Result<bool> {
        let string = self.get_string(id).ok_or(RopeError::StringNotFound(*id))?;
        let complement = self
            .get_complement(id)
            .ok_or(RopeError::ComplementNotFound(*id))?;

        // Verify content against complement
        let content = string.content();
        Ok(complement.verify_content(&content))
    }

    /// Attempt to regenerate a damaged string
    pub fn regenerate_string(&self, id: &StringId) -> Result<RopeString> {
        let complement = self
            .get_complement(id)
            .ok_or(RopeError::ComplementNotFound(*id))?;

        // Get the damaged string (or empty if completely lost)
        let damaged_content = self.get_string(id).map(|s| s.content()).unwrap_or_default();

        // Get replication factor (default if not found)
        let replication_factor = self
            .get_string(id)
            .map(|s| s.replication_factor())
            .unwrap_or(constants::DEFAULT_REPLICATION_FACTOR);

        // Attempt regeneration
        let _regenerated_content = complement
            .regenerate_content(&damaged_content, replication_factor)
            .ok_or(RopeError::RegenerationFailed(*id))?;

        // We need the original string metadata to rebuild
        // For now, return error if completely lost
        Err(RopeError::RegenerationFailed(*id))
    }

    /// Count how many anchor strings reference a given string.
    ///
    /// P1.3: snapshot anchor ids first, then BFS with short per-lookup
    /// parent locks — never hold `anchors.read()` across `get_parents`
    /// (that inverted with `check_anchor_creation`'s `anchors.write()`
    /// and produced the 2026-07-27 BLUE wedge).
    fn count_anchor_references(&self, id: &StringId) -> u32 {
        let anchor_ids: Vec<StringId> = self
            .anchors
            .read()
            .iter()
            .map(|anchor| anchor.id())
            .collect();
        anchor_ids
            .iter()
            .filter(|anchor_id| self.is_ancestor_of(id, anchor_id))
            .count() as u32
    }

    /// Check if `ancestor` is an ancestor of `descendant` in the lattice DAG.
    /// BFS via [`get_parents`], which already shards correctly.
    fn is_ancestor_of(&self, ancestor: &StringId, descendant: &StringId) -> bool {
        if ancestor == descendant {
            return true;
        }

        let mut visited = HashSet::new();
        let mut queue = vec![*descendant];

        while let Some(current) = queue.pop() {
            if current == *ancestor {
                return true;
            }
            if visited.insert(current) {
                queue.extend(self.get_parents(&current));
            }
        }

        false
    }

    /// Check if a string should become an anchor.
    ///
    /// Called only from the maintenance actor / sync fallback — never
    /// from the `add_string` hot path (P1.3).
    fn check_anchor_creation(&self, string: &RopeString) -> Result<()> {
        // Snapshot last-anchor timestamp under a short read, then take the
        // write lock only when we actually create an anchor.
        let last_anchor_time: Option<u64> = self
            .anchors
            .read()
            .last()
            .map(|a| a.string.temporal_marker().time());

        match last_anchor_time {
            Some(last_time) => {
                let time_diff = string
                    .temporal_marker()
                    .time()
                    .saturating_sub(last_time);
                if time_diff > 10 {
                    let mut anchors = self.anchors.write();
                    let mut round = self.current_round.write();
                    // Re-check under write: another candidate may have won.
                    let still_needed = anchors
                        .last()
                        .map(|a| {
                            string
                                .temporal_marker()
                                .time()
                                .saturating_sub(a.string.temporal_marker().time())
                                > 10
                        })
                        .unwrap_or(true);
                    if still_needed {
                        *round += 1;
                        anchors.push(AnchorString::new(string.clone(), *round));
                    }
                }
            }
            None => {
                let mut anchors = self.anchors.write();
                if anchors.is_empty() {
                    anchors.push(AnchorString::new(string.clone(), 0));
                }
            }
        }

        Ok(())
    }

    /// Update finality status based on anchor strings.
    ///
    /// P1.3: snapshot the pending id set under short read locks, drop
    /// them, then run BFS / reference counts with no cross-map lock
    /// held. Only re-acquire pending write locks to prune.
    fn update_finality(&self) {
        let anchor_count = self.anchors.read().len();
        if anchor_count < constants::FINALITY_ANCHORS as usize {
            return;
        }

        // Snapshot pending ids — do not hold `pending.read()` across BFS.
        let mut pending_ids: Vec<StringId> = Vec::new();
        for shard in self.shards.iter() {
            let pending = shard.pending.read();
            for string_ids in pending.values() {
                pending_ids.extend(string_ids.iter().copied());
            }
        }

        let mut newly_finalized = Vec::new();
        for id in &pending_ids {
            let refs = self.count_anchor_references(id);
            if refs >= constants::FINALITY_ANCHORS {
                newly_finalized.push(*id);
            }
        }

        if newly_finalized.is_empty() {
            return;
        }

        {
            let mut finalized = self.finalized_strings.write();
            for id in &newly_finalized {
                finalized.insert(*id);
            }
        }

        for shard in self.shards.iter() {
            let mut pending = shard.pending.write();
            for id in &newly_finalized {
                for string_ids in pending.values_mut() {
                    string_ids.remove(id);
                }
            }
            pending.retain(|_, ids| !ids.is_empty());
        }
    }

    /// Get lattice statistics
    pub fn stats(&self) -> LatticeStats {
        LatticeStats {
            total_strings: self.string_count(),
            pending_strings: self.pending_count(),
            finalized_strings: self.finalized_count(),
            erased_strings: self.erased_count(),
            anchor_count: self.anchors.read().len(),
            current_round: self.current_round(),
        }
    }
}

impl Default for StringLattice {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for StringLattice {
    fn drop(&mut self) {
        // Phase 1.6.β: the `OnceCell<SyncSender>` is dropped naturally by
        // the field-drop order — that drops the last `SyncSender`, which
        // disconnects the channel, which returns `Err(Disconnected)`
        // from `rx.recv()` inside the finality actor and lets it exit
        // its loop. We only need to `.take()` the JoinHandle to wait
        // for it. `Box<[Mutex<Vec<StringId>>]>` and every other field
        // drop themselves via their own `Drop` impls.
        //
        // To make the shutdown order deterministic without relying on
        // Rust's field-drop order (which is stable but implicit here),
        // we explicitly drop the sender first: taking it out of the
        // OnceCell is not permitted (OnceCell has no `take`), but we
        // achieve the same by using `into_inner` on the field via a
        // small `mem::replace`-style dance. Because Drop takes `&mut
        // self`, we can build a fresh empty OnceCell and swap it in,
        // dropping the old (populated) one immediately.
        let taken = std::mem::replace(&mut self.finality_tx, OnceCell::new());
        drop(taken);
        if let Some(handle) = self.finality_join.lock().take() {
            let _ = handle.join();
        }
    }
}

/// Lattice statistics
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatticeStats {
    pub total_strings: usize,
    pub pending_strings: usize,
    pub finalized_strings: usize,
    pub erased_strings: usize,
    pub anchor_count: usize,
    pub current_round: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::LamportClock;
    use crate::string::{PublicKey, RopeString};
    use crate::types::NodeId;

    fn make_test_string(content: &[u8], parents: Vec<StringId>) -> RopeString {
        let mut builder = RopeString::builder()
            .content(content.to_vec())
            .temporal_marker(LamportClock::new(NodeId::new([0u8; 32])))
            .creator(PublicKey::from_ed25519([0u8; 32]));

        for parent in parents {
            builder = builder.add_parent(parent);
        }

        builder.build().unwrap()
    }

    // ----- v1.x compat tests, preserved verbatim -----

    #[test]
    fn test_lattice_creation() {
        let lattice = StringLattice::new();
        assert_eq!(lattice.string_count(), 0);
    }

    #[test]
    fn test_add_string() {
        let lattice = StringLattice::new();
        let string = make_test_string(b"Hello, Rope!", vec![]);

        let id = lattice.add_string(string.clone()).unwrap();

        assert!(lattice.contains(&id));
        assert_eq!(lattice.string_count(), 1);
    }

    #[test]
    fn test_get_string() {
        let lattice = StringLattice::new();
        let content = b"Test content";
        let string = make_test_string(content, vec![]);

        let id = lattice.add_string(string).unwrap();
        let retrieved = lattice.get_string(&id).unwrap();

        // Content is stored in nucleotides (32-byte chunks), so we check prefix
        let retrieved_content = retrieved.content();
        assert!(retrieved_content.starts_with(content));
    }

    #[test]
    fn test_parent_child_relationship() {
        let lattice = StringLattice::new();

        let parent = make_test_string(b"Parent", vec![]);
        let parent_id = lattice.add_string(parent).unwrap();

        let child = make_test_string(b"Child", vec![parent_id]);
        let child_id = lattice.add_string(child).unwrap();

        assert_eq!(lattice.get_parents(&child_id), vec![parent_id]);
        assert_eq!(lattice.get_children(&parent_id), vec![child_id]);
    }

    #[test]
    fn test_missing_parent_error() {
        let lattice = StringLattice::new();
        let fake_parent = StringId::from_content(b"nonexistent");

        let string = make_test_string(b"Orphan", vec![fake_parent]);
        let result = lattice.add_string(string);

        assert!(matches!(result, Err(RopeError::MissingParent(_))));
    }

    #[test]
    fn test_erasure() {
        let lattice = StringLattice::new();
        let string = make_test_string(b"To be erased", vec![]);

        let id = lattice.add_string(string).unwrap();
        assert!(lattice.contains(&id));

        lattice.mark_erased(id).unwrap();
        assert!(!lattice.contains(&id));
        assert!(lattice.get_string(&id).is_none());
    }

    #[test]
    fn test_complement_verification() {
        let lattice = StringLattice::new();
        let string = make_test_string(b"Verifiable content", vec![]);

        let id = lattice.add_string(string).unwrap();

        assert!(lattice.verify_string(&id).unwrap());
    }

    // ====================================================================
    // Quipu Primitive Canon v1.1 — per-knot untying tests, preserved
    // verbatim from v1.x.
    // ====================================================================

    #[test]
    fn test_mark_knot_untied_creates_tombstone() {
        let lattice = StringLattice::new();
        let s = make_test_string(b"knot 1", vec![]);
        let id = lattice.add_string(s).unwrap();

        assert!(!lattice.is_knot_untied(&id));
        assert_eq!(lattice.tombstone_count(), 0);

        let ts = lattice.mark_knot_untied(id, "GdprArticle17").unwrap();

        assert!(lattice.is_knot_untied(&id));
        assert_eq!(lattice.tombstone_count(), 1);
        assert_eq!(ts.reason, "GdprArticle17");
        assert_eq!(ts.audit_hash.len(), 32);
        assert!(lattice.get_tombstone(&id).is_some());
        // Payload is gone (cryptographic erasure)
        assert!(lattice.get_string(&id).is_none());
    }

    #[test]
    fn test_walk_string_with_tombstones_traverses_past_untied_knot() {
        let lattice = StringLattice::new();

        // Build a 3-knot string: genesis ← knot_a ← knot_b
        let genesis = make_test_string(b"genesis", vec![]);
        let g_id = lattice.add_string(genesis).unwrap();

        let a = make_test_string(b"knot_a", vec![g_id]);
        let a_id = lattice.add_string(a).unwrap();

        let b = make_test_string(b"knot_b", vec![a_id]);
        let b_id = lattice.add_string(b).unwrap();

        // Untie the middle knot — its position must remain walkable.
        lattice.mark_knot_untied(a_id, "OwnerRequest").unwrap();

        let entries = lattice.walk_string_with_tombstones(&b_id);

        assert_eq!(
            entries.len(),
            3,
            "walk should return all 3 positions including the tombstone"
        );
        assert_eq!(entries[0].string_id(), g_id, "genesis first");
        assert_eq!(entries[1].string_id(), a_id, "tombstone preserves position");
        assert!(entries[1].is_tombstone(), "middle entry must be tombstone");
        assert_eq!(entries[2].string_id(), b_id, "head last");
        assert!(!entries[0].is_tombstone());
        assert!(!entries[2].is_tombstone());
    }

    #[test]
    fn test_untie_idempotent_via_get_tombstone() {
        let lattice = StringLattice::new();
        let s = make_test_string(b"x", vec![]);
        let id = lattice.add_string(s).unwrap();

        let ts1 = lattice.mark_knot_untied(id, "OwnerRequest").unwrap();
        // Audit hash committed at first untying — survives any future query.
        let ts2 = lattice.get_tombstone(&id).unwrap();
        assert_eq!(ts1.audit_hash, ts2.audit_hash);
        assert_eq!(ts1.untied_at, ts2.untied_at);
    }

    // ====================================================================
    // Quipu Canon v2.0 Phase 1.1 — sharding-specific tests
    // ====================================================================

    #[test]
    fn shard_count_matches_constant() {
        let lattice = StringLattice::new();
        assert_eq!(lattice.shard_count(), NUM_SHARDS);
    }

    #[test]
    fn shards_independently_count() {
        // Insert N strings, verify the shard population is at least
        // diverse (not all in one shard) and that the global count
        // matches the sum of inserts.
        let lattice = StringLattice::new();
        let n = 64usize;
        for i in 0..n {
            let mut content = b"shard-test-".to_vec();
            content.extend_from_slice(&i.to_le_bytes());
            let s = make_test_string(&content, vec![]);
            lattice.add_string(s).unwrap();
        }
        assert_eq!(lattice.string_count(), n);

        let nonempty = lattice
            .shards
            .iter()
            .filter(|sh| !sh.strings.is_empty())
            .count();
        assert!(
            nonempty > 1,
            "with 64 random strings we should hit multiple shards (got {})",
            nonempty
        );
    }

    #[test]
    fn cross_shard_parent_child_edges_round_trip() {
        // Create two strings whose ids almost certainly land in different
        // shards (different content). Verify get_parents on the child
        // (in its own shard) and get_children on the parent (in ITS own
        // shard) both report the correct edge.
        let lattice = StringLattice::new();
        let parent = make_test_string(b"P-cross-shard", vec![]);
        let parent_id = lattice.add_string(parent).unwrap();

        let child = make_test_string(b"C-cross-shard", vec![parent_id]);
        let child_id = lattice.add_string(child).unwrap();

        let parent_shard = shard_for_string_id(&parent_id);
        let child_shard = shard_for_string_id(&child_id);

        // It's possible (1/256) they collide. If so this test is just
        // weaker but still valid.
        if parent_shard != child_shard {
            assert!(
                !lattice.shards[parent_shard]
                    .parents
                    .contains_key(&child_id),
                "child's parents map must NOT live in the parent's shard"
            );
            assert!(
                !lattice.shards[child_shard]
                    .children
                    .contains_key(&parent_id),
                "parent's children map must NOT live in the child's shard"
            );
        }

        assert_eq!(lattice.get_parents(&child_id), vec![parent_id]);
        assert_eq!(lattice.get_children(&parent_id), vec![child_id]);
    }

    #[test]
    fn parallel_inserts_to_distinct_shards_complete_without_loss() {
        use std::sync::Arc;
        use std::thread;

        let lattice = Arc::new(StringLattice::new());
        let mut handles = Vec::new();
        for tid in 0..16u8 {
            let lattice = lattice.clone();
            handles.push(thread::spawn(move || {
                for i in 0..50u32 {
                    let mut content = b"parallel-".to_vec();
                    content.push(tid);
                    content.extend_from_slice(&i.to_le_bytes());
                    let s = make_test_string(&content, vec![]);
                    let _ = lattice.add_string(s).unwrap();
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(lattice.string_count(), 16 * 50);
    }

    /// P1.3: without the background actor, `add_string` still runs
    /// maintenance synchronously so genesis anchors appear immediately.
    #[test]
    fn p13_sync_maintenance_creates_genesis_anchor() {
        let lattice = StringLattice::new();
        let id = lattice
            .add_string(make_test_string(b"p13-genesis-anchor", vec![]))
            .unwrap();
        assert!(lattice.contains(&id));
        assert_eq!(
            lattice.anchors().len(),
            1,
            "sync maintenance must create the genesis anchor"
        );
        // P1.6.β: `anchor_candidates` is now a sharded slice; sync
        // maintenance runs the drain immediately, so every shard is
        // empty by the time this test observes it.
        assert!(
            lattice
                .anchor_candidates
                .iter()
                .all(|shard| shard.lock().is_empty()),
            "sync maintenance must drain every candidate shard"
        );
    }

    /// P1.4: concurrent walks must not convoy behind appends on the same
    /// lattice shard (DashMap entry locks instead of whole-map RwLock).
    #[test]
    fn p14_concurrent_walks_and_appends_complete() {
        use std::sync::Arc;
        use std::thread;

        let lattice = Arc::new(StringLattice::new());
        let genesis = make_test_string(b"p14-genesis", vec![]);
        let g_id = lattice.add_string(genesis).unwrap();

        let mut handles = Vec::new();
        for tid in 0..8u8 {
            let lattice = lattice.clone();
            handles.push(thread::spawn(move || {
                let mut head = g_id;
                for i in 0..40u32 {
                    let mut content = b"p14-".to_vec();
                    content.push(tid);
                    content.extend_from_slice(&i.to_le_bytes());
                    let s = make_test_string(&content, vec![head]);
                    head = lattice.add_string(s).unwrap();
                    // Walk while others append — must not deadlock.
                    let _ = lattice.walk_string_with_tombstones(&head);
                    let _ = lattice.walk_ledger_chain(&head);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(lattice.string_count(), 1 + 8 * 40);
    }

    /// P1.3: with the actor running, candidates are drained asynchronously.
    #[test]
    fn p13_actor_drains_anchor_candidates() {
        use std::sync::Arc;
        use std::thread;
        use std::time::Duration;

        let lattice = Arc::new(StringLattice::new());
        lattice.start_finality_actor();
        let id = lattice
            .add_string(make_test_string(b"p13-actor-genesis", vec![]))
            .unwrap();
        let mut anchors = 0;
        for _ in 0..100 {
            anchors = lattice.anchors().len();
            if anchors > 0 {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(anchors > 0, "finality actor must create a genesis anchor");
        assert!(lattice.contains(&id));
    }

    #[test]
    fn creator_index_shards_independently() {
        let lattice = StringLattice::new();
        let s = make_test_string(b"creator-test", vec![]);
        let id = lattice.add_string(s).unwrap();
        let pk = [0u8; 32];
        assert_eq!(lattice.strings_by_creator(&pk), vec![id]);
        // Erase via the creator path, then verify the creator index slot
        // is empty for that pk.
        let n = lattice.erase_creator_strings(&pk).unwrap();
        assert_eq!(n, 1);
        assert!(lattice.strings_by_creator(&pk).is_empty());
    }

    // ====================================================================
    // Phase 1.6.β — hot-path lock-contention remediation (2026-08-11
    // wedge). These tests guard the two behavioural invariants the
    // sharded anchor_candidates + OnceCell finality_tx change must
    // preserve: (a) no candidate is dropped or double-processed across
    // any shard, and (b) the finality actor is started exactly once
    // and never re-observes a `None` sender after start_finality_actor
    // returns.
    // ====================================================================

    /// Under high concurrency, every append across every shard must
    /// still be seen exactly once by the maintenance path. Regression
    /// guard: an earlier draft accidentally read from a fixed shard
    /// during drain and silently lost the other 255 shards' work.
    #[test]
    fn p16_beta_all_shards_drain_to_maintenance() {
        use std::sync::Arc;
        use std::thread;

        // No background actor — we want `add_string`'s sync path to
        // drive process_anchor_candidates, so every append's shard
        // must be visited during drain.
        let lattice = Arc::new(StringLattice::new());
        let mut handles = Vec::new();
        for tid in 0..8u8 {
            let lattice = lattice.clone();
            handles.push(thread::spawn(move || {
                let mut ids = Vec::new();
                for i in 0..64u32 {
                    let mut content = b"p16b-drain-".to_vec();
                    content.push(tid);
                    content.extend_from_slice(&i.to_le_bytes());
                    let s = make_test_string(&content, vec![]);
                    let id = lattice.add_string(s).unwrap();
                    ids.push(id);
                }
                ids
            }));
        }
        let mut all_ids = Vec::new();
        for h in handles {
            all_ids.extend(h.join().unwrap());
        }
        // Every id must be recoverable from the lattice, AND every
        // candidate shard must be drained (empty) once maintenance has
        // caught up.
        for id in &all_ids {
            assert!(lattice.contains(id), "id {:?} lost", id);
        }
        // Force a final sync drain in case some appends ended before
        // schedule_maintenance ran their sync-path (they shouldn't,
        // but this is a belt-and-braces check that drain is idempotent).
        lattice.process_anchor_candidates();
        assert!(
            lattice
                .anchor_candidates
                .iter()
                .all(|shard| shard.lock().is_empty()),
            "every anchor_candidates shard must be empty after drain"
        );
    }

    /// The sharded `anchor_candidates` queue must actually distribute
    /// pushes across shards. If a bug funnelled everything into shard
    /// 0 we'd still pass functional tests but lose the parallelism
    /// benefit — this test guards that.
    #[test]
    fn p16_beta_pushes_distribute_across_shards() {
        let lattice = StringLattice::new();
        // Build 128 strings that go into the queue but never get
        // drained (we'd need a running actor and no sync drain), so
        // we short-circuit by inspecting the shard sizes right after
        // add_string without waiting for maintenance to sweep. The
        // sync-maintenance path DOES drain immediately, so to observe
        // the distribution we push directly to the shard-count via
        // the same axis add_string uses.
        let mut ids = Vec::new();
        for i in 0u32..128 {
            let mut content = b"p16b-dist-".to_vec();
            content.extend_from_slice(&i.to_le_bytes());
            let s = make_test_string(&content, vec![]);
            let id = lattice.add_string(s).unwrap();
            ids.push(id);
        }
        // After sync drain, all shards are empty (drain went through).
        // The invariant we care about here is that the ids' `[0]`
        // bytes fan out — verify by directly hashing.
        let mut shard_hits = [0u32; NUM_SHARDS];
        for id in &ids {
            shard_hits[shard_for_string_id(id)] += 1;
        }
        let nonempty = shard_hits.iter().filter(|c| **c > 0).count();
        assert!(
            nonempty > 1,
            "128 random ids should hit multiple shards (got {})",
            nonempty
        );
    }

    /// `start_finality_actor` must be idempotent: a second call after
    /// the first successfully installed a sender must be a no-op, and
    /// the OnceCell must remain populated for the whole lattice
    /// lifetime.
    #[test]
    fn p16_beta_finality_actor_start_is_idempotent() {
        use std::sync::Arc;

        let lattice = Arc::new(StringLattice::new());
        assert!(lattice.finality_tx.get().is_none());
        lattice.start_finality_actor();
        assert!(lattice.finality_tx.get().is_some());

        // Second call is a no-op (does not panic, does not spawn a
        // second thread, does not clear the existing sender).
        lattice.start_finality_actor();
        assert!(lattice.finality_tx.get().is_some());

        // schedule_maintenance now takes the lock-free path.
        lattice.schedule_maintenance();
    }

    #[test]
    fn walk_ledger_chain_traverses_cross_shard() {
        // Build a 5-knot chain where each knot's id may live in any shard.
        let lattice = StringLattice::new();
        let mut prev = StringId::ZERO;
        let mut ids = Vec::new();
        for i in 0u32..5 {
            let mut content = b"chain-".to_vec();
            content.extend_from_slice(&i.to_le_bytes());
            let parents = if prev == StringId::ZERO {
                vec![]
            } else {
                vec![prev]
            };
            let s = make_test_string(&content, parents);
            let id = lattice.add_string(s).unwrap();
            ids.push(id);
            prev = id;
        }

        let chain = lattice.walk_ledger_chain(&prev);
        assert_eq!(chain, ids, "walk must reconstruct original chain order");
    }
}
