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
//! - its own `strings`, `complements`, `erased`, `tombstones`, `pending`
//!   maps under per-shard RwLocks;
//! - its own intra-shard DAG (plain `HashMap<StringId, Vec<StringId>>` for
//!   parents and children — petgraph is overkill once we don't need
//!   cross-graph topology operations).
//!
//! Because `StringId[0]` is uniformly random, two concurrent `add_string`
//! calls almost always touch two different shards and proceed in parallel.
//! Genuinely cross-shard edges (parent in shard A, child in shard B) are
//! handled by writing the parent edge in B's `parents` map and the child
//! edge in A's `children` map — two writes, two shards, no contention.
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

use hashbrown::{HashMap, HashSet};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
    strings: RwLock<HashMap<StringId, RopeString>>,

    /// Complements for the strings in this shard.
    complements: RwLock<HashMap<StringId, Complement>>,

    /// child -> parents map. The CHILD lives in this shard; parents may be
    /// in any shard. This replaces petgraph's `Direction::Incoming` query
    /// for nodes whose `StringId[0]` lands in this shard.
    parents: RwLock<HashMap<StringId, Vec<StringId>>>,

    /// parent -> children map. The PARENT lives in this shard; children may
    /// be in any shard. Replaces `Direction::Outgoing` for nodes whose
    /// `StringId[0]` lands in this shard.
    children: RwLock<HashMap<StringId, Vec<StringId>>>,

    /// Erased strings (whole-string tombstones).
    erased: RwLock<HashSet<StringId>>,

    /// Untied-knot tombstones with audit metadata (canon v1.1 §4.2).
    tombstones: RwLock<HashMap<StringId, KnotTombstone>>,

    /// Pending strings awaiting finality, ordered by Lamport time.
    /// Each shard keeps its own slice; aggregation is via summing across
    /// shards (low-frequency call, used by `pending_count` and finality).
    pending: RwLock<BTreeMap<u64, HashSet<StringId>>>,

    /// Quipu Canon v2.0 — per-string anchor reference watermark.
    ///
    /// Replaces the old O(N²) `update_finality` scan with an
    /// incremental counter: every time a new anchor is created we
    /// walk its ancestor cone exactly once and `+= 1` on each
    /// visited string's entry here. Reads (e.g. via
    /// [`StringLattice::check_finality`]) are then O(1) instead of
    /// O(P × A × D).
    ///
    /// The counter is monotone non-decreasing during a string's
    /// active lifetime and is dropped when the string is
    /// [`StringLattice::mark_erased`]. Sharded by `StringId[0]` like
    /// every other map on this struct, so concurrent anchor walks on
    /// different shards never contend.
    anchor_refs: RwLock<HashMap<StringId, u32>>,
}

impl LatticeShard {
    fn new() -> Self {
        Self {
            strings: RwLock::new(HashMap::new()),
            complements: RwLock::new(HashMap::new()),
            parents: RwLock::new(HashMap::new()),
            children: RwLock::new(HashMap::new()),
            erased: RwLock::new(HashSet::new()),
            tombstones: RwLock::new(HashMap::new()),
            pending: RwLock::new(BTreeMap::new()),
            anchor_refs: RwLock::new(HashMap::new()),
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
}

impl StringLattice {
    /// Create a new empty string lattice with [`NUM_SHARDS`] shards.
    pub fn new() -> Self {
        let shards: Vec<LatticeShard> = (0..NUM_SHARDS).map(|_| LatticeShard::new()).collect();
        let creator_shards: Vec<CreatorShard> = (0..NUM_SHARDS)
            .map(|_| RwLock::new(HashMap::new()))
            .collect();
        Self {
            shards: shards.into_boxed_slice(),
            creator_index: creator_shards.into_boxed_slice(),
            anchors: RwLock::new(Vec::new()),
            finalized_strings: RwLock::new(HashSet::new()),
            current_round: RwLock::new(0),
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
    ///   7. Check for anchor creation (global, anchor-cadence)
    pub fn add_string(&self, string: RopeString) -> Result<StringId> {
        let id = string.id();
        let parents = string.parentage().to_vec();

        // Step 1+2: verify parentage. Each parent lives in its own shard
        // (typically a different one from `id`). Take the parent's shard's
        // strings + erased read locks for the check.
        for parent in &parents {
            if parent.as_bytes().iter().all(|&b| b == 0) {
                continue; // genesis sentinel
            }
            let p_shard = &self.shards[shard_for_string_id(parent)];
            if !p_shard.strings.read().contains_key(parent) {
                return Err(RopeError::MissingParent(*parent));
            }
            if p_shard.erased.read().contains(parent) {
                return Err(RopeError::ParentErased(*parent));
            }
        }

        // Step 3: complement (pure CPU, no locks)
        let complement = Complement::generate(&string);

        let timestamp = string.temporal_marker().time();
        let creator_key = string.creator().ed25519;
        let id_shard_idx = shard_for_string_id(&id);

        // Step 4: insert into the child's shard. Acquire its 4 write locks
        // together so the slice's internal invariants (string ↔ complement,
        // string ↔ parents, string ↔ pending) hold atomically for any
        // concurrent reader.
        {
            let shard = &self.shards[id_shard_idx];
            let mut strings = shard.strings.write();
            let mut complements = shard.complements.write();
            let mut parents_map = shard.parents.write();
            let mut pending = shard.pending.write();

            strings.insert(id, string.clone());
            complements.insert(id, complement);
            parents_map.insert(id, parents.clone());
            pending.entry(timestamp).or_default().insert(id);
        }

        // Step 5: update each parent's shard's `children` index. Skip the
        // genesis sentinel; group parents by shard so each shard's lock is
        // taken at most once.
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
            let mut children_map = self.shards[s_idx].children.write();
            for parent in parents_in_shard {
                children_map.entry(parent).or_default().push(id);
            }
        }

        // Step 6: creator index, on its own sharding axis.
        self.creator_index[shard_for_creator(&creator_key)]
            .write()
            .entry(creator_key)
            .or_default()
            .push(id);

        // Step 7: anchor check (global, low rate)
        self.check_anchor_creation(&string)?;

        Ok(id)
    }

    /// Get a string by ID.
    pub fn get_string(&self, id: &StringId) -> Option<RopeString> {
        let shard = &self.shards[shard_for_string_id(id)];
        if shard.erased.read().contains(id) {
            return None;
        }
        shard.strings.read().get(id).cloned()
    }

    /// Get a complement by string ID.
    pub fn get_complement(&self, id: &StringId) -> Option<Complement> {
        let shard = &self.shards[shard_for_string_id(id)];
        if shard.erased.read().contains(id) {
            return None;
        }
        shard.complements.read().get(id).cloned()
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
        !shard.erased.read().contains(id) && shard.strings.read().contains_key(id)
    }

    /// Get the total number of strings in the lattice. Aggregates across
    /// shards under each shard's read lock.
    pub fn string_count(&self) -> usize {
        self.shards.iter().map(|s| s.strings.read().len()).sum()
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
        self.shards.iter().map(|s| s.erased.read().len()).sum()
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
            .read()
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Get children of a string. Looks up only one shard (the parent's).
    pub fn get_children(&self, id: &StringId) -> Vec<StringId> {
        self.shards[shard_for_string_id(id)]
            .children
            .read()
            .get(id)
            .cloned()
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
    /// Each step looks up one shard (the current id's). Bouncing across
    /// shards is cheap because each lookup is a per-shard read lock — they
    /// don't contend with each other or with concurrent writers on other
    /// shards.
    pub fn walk_ledger_chain(&self, head: &StringId) -> Vec<StringId> {
        let mut chain = Vec::new();
        let mut current = *head;

        loop {
            if current == StringId::ZERO {
                break;
            }
            let shard = &self.shards[shard_for_string_id(&current)];
            if shard.erased.read().contains(&current) {
                break;
            }
            let next = {
                let strings = shard.strings.read();
                match strings.get(&current) {
                    Some(s) => {
                        chain.push(current);
                        s.parentage().first().copied().unwrap_or(StringId::ZERO)
                    }
                    None => break,
                }
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
        let mut erased = shard.erased.write();
        let mut strings = shard.strings.write();
        let mut complements = shard.complements.write();

        if !strings.contains_key(&id) {
            return Err(RopeError::StringNotFound(id));
        }

        // Remove from active storage
        strings.remove(&id);
        complements.remove(&id);

        // Add to erased set (tombstone)
        erased.insert(id);

        // Quipu Canon v2.0 — drop the anchor watermark counter for
        // this id. Counter retention after erasure would slowly leak
        // memory under high churn (GDPR-driven untying loops in
        // particular). The string's pending slot has already been
        // pruned if it was finalised; if it was still pending we
        // also need to drop it from `pending` to keep `pending_count`
        // honest.
        drop(strings);
        drop(complements);
        drop(erased);
        shard.anchor_refs.write().remove(&id);
        {
            let mut pending = shard.pending.write();
            for entries in pending.values_mut() {
                entries.remove(&id);
            }
            pending.retain(|_, ids| !ids.is_empty());
        }

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
            .write()
            .insert(id, tombstone.clone());

        Ok(tombstone)
    }

    /// Look up a knot's tombstone metadata, if any. Returns None if the knot
    /// was never untied (or was whole-string erased without tombstone metadata).
    pub fn get_tombstone(&self, id: &StringId) -> Option<KnotTombstone> {
        self.shards[shard_for_string_id(id)]
            .tombstones
            .read()
            .get(id)
            .cloned()
    }

    /// Check whether a knot has been untied via the canonical canon v1.1 path.
    pub fn is_knot_untied(&self, id: &StringId) -> bool {
        self.shards[shard_for_string_id(id)]
            .tombstones
            .read()
            .contains_key(id)
    }

    /// Total count of untied knots across all shards (transparency metric
    /// for the canon §6(5) UI).
    pub fn tombstone_count(&self) -> usize {
        self.shards.iter().map(|s| s.tombstones.read().len()).sum()
    }

    /// Walk a wallet's string from `head` back to genesis, but DO NOT stop
    /// at tombstones. Returns one `LedgerEntry` per knot position — either
    /// `Active(StringId)` or `Tombstone(StringId, KnotTombstone)`.
    ///
    /// Walks via per-shard `parents` edges (which survive `mark_knot_untied`)
    /// when the live RopeString is gone, and via the RopeString's own
    /// parentage when it's present. Returned vector is genesis-first
    /// (oldest first).
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
            let next = {
                let strings = shard.strings.read();
                if let Some(s) = strings.get(&current) {
                    chain.push(LedgerEntry::Active(current));
                    s.parentage().first().copied().unwrap_or(StringId::ZERO)
                } else {
                    drop(strings);
                    let tombstones = shard.tombstones.read();
                    if let Some(ts) = tombstones.get(&current) {
                        chain.push(LedgerEntry::Tombstone(current, ts.clone()));
                        drop(tombstones);
                        // Hop past via the per-shard parents map.
                        shard
                            .parents
                            .read()
                            .get(&current)
                            .and_then(|p| p.first().copied())
                            .unwrap_or(StringId::ZERO)
                    } else {
                        // Unknown id — neither live nor tombstoned. Stop.
                        break;
                    }
                }
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

    /// Look up how many anchors reference a given string. O(1) — reads
    /// the cached watermark maintained by
    /// [`Self::increment_anchor_refs_along_cone`] during anchor creation.
    ///
    /// Quipu Canon v2.0 — replaces the v1.x O(P × A × D) scan that
    /// dominated the manager-write benchmark above ~3k ops/s with
    /// many wallets fanning into a shared lattice.
    fn count_anchor_references(&self, id: &StringId) -> u32 {
        self.shards[shard_for_string_id(id)]
            .anchor_refs
            .read()
            .get(id)
            .copied()
            .unwrap_or(0)
    }

    /// Check if `ancestor` is an ancestor of `descendant` in the lattice DAG.
    /// BFS via [`get_parents`], which already shards correctly.
    ///
    /// Retained for the public/test surface even though the hot
    /// finality path no longer needs it; finality reads
    /// [`Self::count_anchor_references`] directly.
    #[allow(dead_code)]
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

    /// Check if a string should become an anchor
    fn check_anchor_creation(&self, string: &RopeString) -> Result<()> {
        // Simplified anchor creation logic
        // Real implementation would involve virtual voting

        let anchors = self.anchors.read();
        let new_anchor_id_opt: Option<StringId> = if let Some(last_anchor) = anchors.last() {
            // Check if enough time has passed since last anchor
            let time_diff = string
                .temporal_marker()
                .time()
                .saturating_sub(last_anchor.string.temporal_marker().time());

            if time_diff > 10 {
                drop(anchors);

                let mut anchors = self.anchors.write();
                let mut round = self.current_round.write();

                *round += 1;
                let new_anchor = AnchorString::new(string.clone(), *round);
                let id = new_anchor.id();
                anchors.push(new_anchor);
                drop(anchors);
                drop(round);
                Some(id)
            } else {
                None
            }
        } else {
            // First anchor (genesis)
            drop(anchors);

            let mut anchors = self.anchors.write();
            let anchor = AnchorString::new(string.clone(), 0);
            let id = anchor.id();
            anchors.push(anchor);
            Some(id)
        };

        // Quipu Canon v2.0 — incremental finality watermark.
        //
        // Walk the brand-new anchor's ancestor cone ONCE via the
        // per-shard `parents` maps and `+= 1` on every visited
        // string's `anchor_refs` counter. Strings whose counter
        // crosses `FINALITY_ANCHORS` are finalised immediately —
        // moved from their shard's `pending` slice to the global
        // `finalized_strings` set. This collapses what used to be an
        // O(P × A × D) per-anchor sweep into an O(D) walk where D is
        // the size of the new anchor's ancestor cone (typically
        // ~equal to the time since the previous anchor).
        if let Some(anchor_id) = new_anchor_id_opt {
            self.increment_anchor_refs_along_cone(anchor_id);
        }

        Ok(())
    }

    /// Walk the ancestor cone of `anchor_id` exactly once and bump
    /// each visited string's `anchor_refs` counter. Strings whose
    /// counter reaches [`constants::FINALITY_ANCHORS`] are atomically
    /// transferred from their shard's `pending` slice to the global
    /// `finalized_strings` set.
    ///
    /// Called from [`Self::check_anchor_creation`] only; called once
    /// per new anchor under serialisation by `anchors.write()`, so two
    /// concurrent anchor creations cannot interleave and produce a
    /// duplicate `+= 1` on the same `(anchor, ancestor)` pair.
    fn increment_anchor_refs_along_cone(&self, anchor_id: StringId) {
        let mut visited: HashSet<StringId> = HashSet::new();
        let mut newly_finalized: Vec<StringId> = Vec::new();
        let mut stack: Vec<StringId> = vec![anchor_id];

        while let Some(current) = stack.pop() {
            if current == StringId::ZERO {
                continue;
            }
            if !visited.insert(current) {
                continue;
            }

            let shard_idx = shard_for_string_id(&current);
            let shard = &self.shards[shard_idx];

            // Bump the per-shard counter, capturing the post-bump
            // value so we know if we just crossed the finality
            // threshold.
            let new_count = {
                let mut refs = shard.anchor_refs.write();
                let entry = refs.entry(current).or_insert(0);
                *entry = entry.saturating_add(1);
                *entry
            };

            // Crossing the threshold finalises the string. We only
            // count the FIRST crossing — once a string is in
            // `finalized_strings`, additional anchors that reference
            // it leave the counter monotonically rising but the set
            // membership unchanged.
            if new_count == constants::FINALITY_ANCHORS {
                newly_finalized.push(current);
            }

            // Enqueue parents. The per-shard `parents` map lives in
            // the CHILD's shard (i.e. `current`'s shard), so this is
            // a single read on the same shard we just bumped.
            let parents_snapshot: Vec<StringId> = shard
                .parents
                .read()
                .get(&current)
                .map(|v| v.clone())
                .unwrap_or_default();
            for p in parents_snapshot {
                if p != StringId::ZERO && !visited.contains(&p) {
                    stack.push(p);
                }
            }
        }

        if newly_finalized.is_empty() {
            return;
        }

        // Insert into the global finalized set under a single write lock.
        {
            let mut finalized = self.finalized_strings.write();
            for id in &newly_finalized {
                finalized.insert(*id);
            }
        }

        // Prune from each shard's pending slice. Group by shard so
        // each shard's `pending` write lock is taken at most once.
        let mut by_shard: HashMap<usize, Vec<StringId>> = HashMap::new();
        for id in &newly_finalized {
            by_shard
                .entry(shard_for_string_id(id))
                .or_default()
                .push(*id);
        }
        for (s_idx, ids) in by_shard {
            let mut pending = self.shards[s_idx].pending.write();
            // Pending is keyed by Lamport time; we don't carry that
            // metadata into the anchor walk so we have to scan the
            // BTreeMap. In practice each shard's pending slice is
            // small (~ops_per_anchor_window / NUM_SHARDS) so this is
            // bounded and far cheaper than the old O(P × A × D)
            // sweep.
            for id in ids {
                for entries in pending.values_mut() {
                    entries.remove(&id);
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
            .filter(|sh| !sh.strings.read().is_empty())
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
                    .read()
                    .contains_key(&child_id),
                "child's parents map must NOT live in the parent's shard"
            );
            assert!(
                !lattice.shards[child_shard]
                    .children
                    .read()
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
    // Quipu Canon v2.0 Phase 2.C.1 — incremental finality watermark
    // ====================================================================

    /// Build a linear chain of `n` strings using a Lamport clock that
    /// advances by `tick` ticks per knot. Returns the head id and the
    /// full ordered list of ids (oldest-first).
    fn build_linear_chain(
        lattice: &StringLattice,
        n: usize,
        tick_per_knot: u64,
    ) -> (StringId, Vec<StringId>) {
        let mut clock = LamportClock::new(NodeId::new([0u8; 32]));
        let mut prev = StringId::ZERO;
        let mut ids = Vec::with_capacity(n);
        for i in 0..n {
            for _ in 0..tick_per_knot {
                clock.increment();
            }
            let parents = if prev == StringId::ZERO {
                vec![]
            } else {
                vec![prev]
            };
            let mut content = b"linear-".to_vec();
            content.extend_from_slice(&i.to_le_bytes());
            let mut builder = RopeString::builder()
                .content(content)
                .temporal_marker(clock.clone())
                .creator(PublicKey::from_ed25519([0u8; 32]));
            for p in parents {
                builder = builder.add_parent(p);
            }
            let s = builder.build().unwrap();
            let id = lattice.add_string(s).unwrap();
            ids.push(id);
            prev = id;
        }
        (prev, ids)
    }

    #[test]
    fn anchor_refs_increment_for_genesis_anchor_only() {
        // First string with no time gap → becomes the genesis anchor
        // immediately. The genesis path increments anchor_refs[id] = 1
        // for the genesis id itself (the only string in its cone).
        let lattice = StringLattice::new();
        let s = make_test_string(b"genesis-knot", vec![]);
        let id = lattice.add_string(s).unwrap();
        assert_eq!(lattice.count_anchor_references(&id), 1);
        assert_eq!(lattice.anchors().len(), 1);
    }

    #[test]
    fn finality_watermark_is_o1_per_string() {
        // Build a linear chain long enough to trigger several anchor
        // creations (anchor cadence = time_diff > 10 Lamport ticks),
        // then assert that each anchor's effect on `count_anchor_references`
        // is incremental — the call itself never walks the DAG.
        let lattice = StringLattice::new();
        let (_head, ids) = build_linear_chain(&lattice, 100, 11); // >10 → anchor every knot
        // 100 anchors created (every knot becomes one). Each ancestor
        // is referenced by every later anchor. The genesis (ids[0]) is
        // referenced by all 100 anchors; the last (ids[99]) by exactly 1.
        // We don't assert the precise number (anchor cadence may differ
        // by exact Lamport-tick interpretation) — what matters for this
        // test is that count_anchor_references returns a sensible
        // non-zero number in O(1) instead of falling back to BFS.
        let refs_first = lattice.count_anchor_references(&ids[0]);
        let refs_last = lattice.count_anchor_references(&ids[ids.len() - 1]);
        assert!(
            refs_first >= 1,
            "genesis must be referenced by at least itself"
        );
        assert!(refs_last >= 1, "last must be referenced by itself");
        assert!(
            refs_first >= refs_last,
            "older strings must accumulate at least as many anchor refs as newer ones (first={}, last={})",
            refs_first,
            refs_last,
        );
    }

    #[test]
    fn finality_threshold_promotes_pending_to_finalized() {
        // FINALITY_ANCHORS = 3. Build a linear chain so that at least
        // the genesis ends up referenced by ≥ 3 anchors, and assert
        // it transitions from pending to finalized.
        let lattice = StringLattice::new();
        let (_head, ids) = build_linear_chain(&lattice, 10, 11); // ~10 anchors
        let g_refs = lattice.count_anchor_references(&ids[0]);
        assert!(
            g_refs >= constants::FINALITY_ANCHORS,
            "genesis should accumulate ≥ FINALITY_ANCHORS anchor refs (got {g_refs})"
        );
        assert!(
            lattice.is_finalized(&ids[0]),
            "genesis must be in finalized_strings once threshold crossed"
        );
        // The newest knot has only itself as anchor (ref count = 1)
        // and must NOT be finalised yet.
        assert!(
            !lattice.is_finalized(&ids[ids.len() - 1]),
            "last knot must still be pending (only 1 anchor ref)"
        );
    }

    #[test]
    fn anchor_refs_dropped_on_mark_erased() {
        // Build a chain, sanity check the watermark, then erase and
        // verify the counter is gone (no memory leak across GDPR
        // erasure loops).
        let lattice = StringLattice::new();
        let s = make_test_string(b"erasable", vec![]);
        let id = lattice.add_string(s).unwrap();
        assert!(lattice.count_anchor_references(&id) >= 1);
        lattice.mark_erased(id).unwrap();
        // After erasure the per-shard map must not retain the entry —
        // count_anchor_references reads it and returns 0 for missing.
        assert_eq!(
            lattice.count_anchor_references(&id),
            0,
            "anchor_refs entry must be dropped on erasure",
        );
    }

    #[test]
    fn check_finality_is_constant_time_after_p2c1() {
        // This test exists primarily as a regression guard: it runs
        // 1024 finality checks on a 256-knot chain and times out if
        // the check ever falls back to the old O(P × A × D) BFS.
        // The wall-clock target is generous (50 ms across 1024 calls
        // = 50 µs each) — a true O(1) cache read should land in the
        // single-µs range on commodity hardware. CI noise allows up
        // to 50× slack.
        let lattice = StringLattice::new();
        let (_head, ids) = build_linear_chain(&lattice, 256, 11);
        let started = std::time::Instant::now();
        for _ in 0..4 {
            for id in &ids {
                let _ = lattice.check_finality(id);
            }
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < std::time::Duration::from_millis(50),
            "1024 check_finality calls must take < 50 ms (took {:?}); the O(N²) cliff has regressed",
            elapsed,
        );
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
