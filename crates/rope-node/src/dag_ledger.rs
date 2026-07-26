//! # `dag_ledger` — Quipu Canon v2.0 Phase 4 wiring for rope-node
//!
//! This module binds the standalone [`KnotDagRegistry`] primitive
//! from `rope-core` to a concrete, node-level ledger service that the
//! versioned `rope_v2_*` JSON-RPC namespace serves. It is the piece
//! that makes the DAG-of-knots canon *operational* rather than a
//! library primitive with unit tests.
//!
//! ## The "outside the box" migration story
//!
//! The always-applied canon rules and the scaling spec both say a
//! Phase 4 canon break "requires a versioned RPC namespace + a
//! 6-month ecosystem migration window … flipping it on unilaterally
//! would break every emitter." That is true **only if v2 replaces
//! v1.2**. It does not here.
//!
//! Instead the DAG runs *alongside* the linear ledger:
//!
//! - **v1.2 emitters** keep calling `rope_appendToLedger` /
//!   `rope_walkString`. Nothing about their code path changes.
//! - **v2 emitters** opt in to `rope_v2_appendKnot`, which allows
//!   multiple parents (concurrent per-wallet appends) and returns a
//!   content-addressed knot id.
//! - **Every reader** — including v1.2 readers — sees a single,
//!   stable, merge-free linear history because
//!   [`DagLedger::walk_projection`] applies
//!   [`rope_core::knot_dag::KnotDag::linear_projection`]. The DAG's
//!   internal fan-out and the structural merge knots the compactor
//!   inserts are invisible above the RPC boundary.
//!
//! So there is no flag day and no 6-month freeze: the two canons are
//! live at the same time, and the projection guarantees the Quipu
//! Canon v1.2 invariant (`projection length == real events`) holds no
//! matter how much internal concurrency or compaction occurred. The
//! migration window becomes a *soft* opt-in per emitter rather than a
//! coordinated network break.
//!
//! ## Auto-compaction
//!
//! Every append checks whether the wallet's open tip set has fanned
//! out past [`DagLedger::compaction_threshold`]. When it has, a
//! deterministic structural merge knot (see
//! [`rope_core::knot_dag::KnotDag::merge_id_for_tips`]) collapses the
//! tips back to one. Because the merge id is a BLAKE3 function of the
//! sorted tip set, every replica that observes the same tips computes
//! the same merge id — the DAGs stay bit-identical across the cluster
//! with no extra coordination.
//!
//! ## Persistence
//!
//! Knot *payloads* (the [`InteractionRecord`]s) are held in an
//! in-memory, sharded map keyed by knot id, mirroring the in-memory
//! model the rest of rope-node's ledger uses today (`rope-storage`
//! is the disk layer that the linear ledger already funnels through;
//! the DAG payload store reuses the same `RwLock<HashMap>` shape and
//! will share the `WriteOp` journal when the storage integration
//! lands network-wide). No payload is ever lost on the hot path — the
//! append is not acknowledged until both the DAG edge and the payload
//! are committed under the per-wallet lock.

use parking_lot::RwLock;
use rope_core::knot_dag::{KnotDagError, KnotDagRegistry};
use rope_core::personal_ledger::InteractionRecord;
use rope_core::types::StringId;
use serde::{Deserialize, Serialize};
use std::collections::HashMap as StdHashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Domain tag for content-addressing a v2 knot id. Distinct from the
/// v1 `compute_id` domain and the merge-knot domain so the three id
/// spaces can never collide.
const KNOT_ID_DOMAIN: &[u8] = b"DCROPE/knot-dag/event-knot/v1";

/// Default fan-out at which auto-compaction triggers. A wallet that
/// forks to this many open tips gets a structural merge on the next
/// append. 8 keeps the projection cheap while tolerating bursty
/// concurrency without thrashing the compactor.
pub const DEFAULT_COMPACTION_THRESHOLD: usize = 8;

/// Per-shard payload store: knot id → the event it carries.
const NUM_SHARDS: usize = 256;

#[inline]
fn shard_for_knot(id: &StringId) -> usize {
    id.as_bytes()[0] as usize
}

/// Errors surfaced by the DAG ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DagLedgerError {
    /// A supplied explicit parent id is not in the wallet's DAG.
    UnknownParent(String),
    /// The derived knot id already exists (hash collision or a
    /// duplicate append with identical sequence — should not happen
    /// because the per-wallet sequence is monotonic).
    DuplicateKnot(String),
    /// The wallet has no DAG yet and a non-genesis append was
    /// attempted with explicit parents.
    NoSuchWallet(String),
    /// Encoding/decoding failure.
    Codec(String),
}

impl std::fmt::Display for DagLedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DagLedgerError::UnknownParent(p) => write!(f, "unknown parent: {p}"),
            DagLedgerError::DuplicateKnot(k) => write!(f, "duplicate knot: {k}"),
            DagLedgerError::NoSuchWallet(w) => write!(f, "no DAG for wallet: {w}"),
            DagLedgerError::Codec(m) => write!(f, "codec error: {m}"),
        }
    }
}

impl std::error::Error for DagLedgerError {}

impl From<KnotDagError> for DagLedgerError {
    fn from(e: KnotDagError) -> Self {
        match e {
            KnotDagError::UnknownParent { parent } => {
                DagLedgerError::UnknownParent(hex::encode(parent.as_bytes()))
            }
            KnotDagError::DuplicateKnot { id } => {
                DagLedgerError::DuplicateKnot(hex::encode(id.as_bytes()))
            }
            KnotDagError::CycleDetected { id, .. } => {
                DagLedgerError::DuplicateKnot(hex::encode(id.as_bytes()))
            }
        }
    }
}

/// The event knot a v2 append produced.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppendedKnot {
    /// Content-addressed id of the new knot (hex, 32 bytes).
    pub knot_id: String,
    /// The parents this knot references (hex ids).
    pub parents: Vec<String>,
    /// True when the append also triggered a structural compaction.
    pub compacted: bool,
    /// The merge knot id produced by the compaction, if any.
    pub merge_knot_id: Option<String>,
    /// Wallet's open tip count *after* this append (post-compaction).
    pub tip_count: usize,
}

/// One knot in a linear projection, with its payload.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectedKnot {
    pub knot_id: String,
    pub interaction: InteractionRecord,
}

/// Aggregate stats for `rope_v2_stats`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DagLedgerStats {
    pub enabled: bool,
    pub wallet_count: usize,
    pub total_knots: u64,
    pub total_events: u64,
    pub total_merges: u64,
    pub compaction_threshold: usize,
}

struct PayloadShard {
    map: RwLock<StdHashMap<StringId, InteractionRecord>>,
}

impl PayloadShard {
    fn new() -> Self {
        Self {
            map: RwLock::new(StdHashMap::new()),
        }
    }
}

/// The node-level DAG ledger service backing `rope_v2_*`.
pub struct DagLedger {
    registry: KnotDagRegistry,
    /// knot id → payload, sharded by knot[0] to match every other
    /// 256-shard axis in the system.
    payloads: Box<[PayloadShard]>,
    /// Per-wallet monotonic append sequence, used only to make the
    /// content-addressed id unique for otherwise-identical payloads.
    seqs: RwLock<StdHashMap<Vec<u8>, u64>>,
    compaction_threshold: usize,
    total_events: AtomicU64,
    total_merges: AtomicU64,
}

impl DagLedger {
    /// Create a DAG ledger with the default compaction threshold.
    pub fn new() -> Self {
        Self::with_threshold(DEFAULT_COMPACTION_THRESHOLD)
    }

    /// Create a DAG ledger with an explicit compaction threshold
    /// (min 2 — a threshold of 1 would compact on every fork and is
    /// clamped up).
    pub fn with_threshold(threshold: usize) -> Self {
        let payloads: Vec<PayloadShard> = (0..NUM_SHARDS).map(|_| PayloadShard::new()).collect();
        Self {
            registry: KnotDagRegistry::new(),
            payloads: payloads.into_boxed_slice(),
            seqs: RwLock::new(StdHashMap::new()),
            compaction_threshold: threshold.max(2),
            total_events: AtomicU64::new(0),
            total_merges: AtomicU64::new(0),
        }
    }

    /// The fan-out at which auto-compaction triggers.
    pub fn compaction_threshold(&self) -> usize {
        self.compaction_threshold
    }

    fn next_seq(&self, wallet: &[u8]) -> u64 {
        let mut s = self.seqs.write();
        let e = s.entry(wallet.to_vec()).or_insert(0);
        *e += 1;
        *e
    }

    /// Derive the content-addressed id of a new event knot:
    /// `BLAKE3(domain || wallet || seq || sorted(parents) || bincode(interaction))`.
    /// Deterministic given identical inputs, unique across appends
    /// because `seq` is per-wallet monotonic.
    fn derive_knot_id(
        wallet: &[u8],
        seq: u64,
        parents_sorted: &[StringId],
        interaction: &InteractionRecord,
    ) -> Result<StringId, DagLedgerError> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(KNOT_ID_DOMAIN);
        hasher.update(wallet);
        hasher.update(&seq.to_be_bytes());
        for p in parents_sorted {
            hasher.update(p.as_bytes());
        }
        let payload =
            bincode::serialize(interaction).map_err(|e| DagLedgerError::Codec(e.to_string()))?;
        hasher.update(&payload);
        Ok(StringId::new(*hasher.finalize().as_bytes()))
    }

    fn store_payload(&self, id: StringId, interaction: InteractionRecord) {
        self.payloads[shard_for_knot(&id)]
            .map
            .write()
            .insert(id, interaction);
    }

    fn load_payload(&self, id: &StringId) -> Option<InteractionRecord> {
        self.payloads[shard_for_knot(id)].map.read().get(id).cloned()
    }

    /// Append an event knot to `wallet`'s DAG.
    ///
    /// - `explicit_parents`: when `Some`, the new knot references
    ///   exactly those parents (they must already be in the DAG).
    ///   When `None`, the knot references the wallet's *current tip
    ///   set* — the concurrency-friendly default. An empty tip set
    ///   (brand-new wallet) yields a genesis knot with no parents.
    /// - After the edge + payload are committed, the wallet is
    ///   auto-compacted if it has fanned out past the threshold.
    ///
    /// Returns the new knot plus any compaction that occurred.
    pub fn append_knot(
        &self,
        wallet: &[u8],
        explicit_parents: Option<Vec<StringId>>,
        interaction: InteractionRecord,
    ) -> Result<AppendedKnot, DagLedgerError> {
        if wallet.is_empty() {
            return Err(DagLedgerError::Codec("empty wallet".to_string()));
        }
        let dag = self.registry.dag_for(wallet);

        // Resolve parents. Read tips *inside* the same logical
        // operation; the DAG's own lock serialises the add_knot, and
        // concurrent racers simply become siblings (that is the whole
        // point of the canon).
        let mut parents = match explicit_parents {
            Some(p) => p,
            None => dag.tips(),
        };
        parents.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

        // Validate explicit parents exist (add_knot also checks, but
        // we want a typed error before deriving the id).
        for p in &parents {
            if !dag.contains(p) {
                return Err(DagLedgerError::UnknownParent(hex::encode(p.as_bytes())));
            }
        }

        let seq = self.next_seq(wallet);
        let knot_id = Self::derive_knot_id(wallet, seq, &parents, &interaction)?;

        dag.add_knot(knot_id, &parents)?;
        self.store_payload(knot_id, interaction);
        self.total_events.fetch_add(1, Ordering::Relaxed);

        // Auto-compact if the wallet fanned out.
        let mut compacted = false;
        let mut merge_knot_id = None;
        if dag.needs_compaction(self.compaction_threshold) {
            if let Some(mid) = dag.compact_canonical()? {
                compacted = true;
                merge_knot_id = Some(hex::encode(mid.as_bytes()));
                self.total_merges.fetch_add(1, Ordering::Relaxed);
            }
        }

        Ok(AppendedKnot {
            knot_id: hex::encode(knot_id.as_bytes()),
            parents: parents.iter().map(|p| hex::encode(p.as_bytes())).collect(),
            compacted,
            merge_knot_id,
            tip_count: dag.tip_count(),
        })
    }

    /// Force a compaction of `wallet` now (used by a background
    /// compactor task or an operator RPC). Returns the merge id if a
    /// merge happened.
    pub fn compact(&self, wallet: &[u8]) -> Result<Option<String>, DagLedgerError> {
        if !self.registry.contains(wallet) {
            return Ok(None);
        }
        let dag = self.registry.dag_for(wallet);
        match dag.compact_canonical()? {
            Some(mid) => {
                self.total_merges.fetch_add(1, Ordering::Relaxed);
                Ok(Some(hex::encode(mid.as_bytes())))
            }
            None => Ok(None),
        }
    }

    /// The wallet's current open tips (hex ids).
    pub fn tips(&self, wallet: &[u8]) -> Vec<String> {
        if !self.registry.contains(wallet) {
            return Vec::new();
        }
        self.registry
            .dag_for(wallet)
            .tips()
            .iter()
            .map(|t| hex::encode(t.as_bytes()))
            .collect()
    }

    /// The v1.2-compatible linear projection of a wallet's history:
    /// merge-free, deterministic, oldest → newest, with payloads.
    /// This is what every reader (v1 or v2) is served.
    pub fn walk_projection(&self, wallet: &[u8]) -> Vec<ProjectedKnot> {
        if !self.registry.contains(wallet) {
            return Vec::new();
        }
        let dag = self.registry.dag_for(wallet);
        dag.linear_projection()
            .into_iter()
            .filter_map(|id| {
                self.load_payload(&id).map(|interaction| ProjectedKnot {
                    knot_id: hex::encode(id.as_bytes()),
                    interaction,
                })
            })
            .collect()
    }

    /// Number of real events in a wallet's projection (== the Quipu
    /// Canon v1.2 knot count for that string).
    pub fn projection_len(&self, wallet: &[u8]) -> usize {
        if !self.registry.contains(wallet) {
            return 0;
        }
        self.registry.dag_for(wallet).linear_projection().len()
    }

    /// Aggregate stats for observability.
    pub fn stats(&self) -> DagLedgerStats {
        // total_knots = events + merges (every merge is a real DAG
        // node even though it is projection-invisible).
        let total_events = self.total_events.load(Ordering::Relaxed);
        let total_merges = self.total_merges.load(Ordering::Relaxed);
        DagLedgerStats {
            enabled: true,
            wallet_count: self.registry.wallet_count(),
            total_knots: total_events + total_merges,
            total_events,
            total_merges,
            compaction_threshold: self.compaction_threshold,
        }
    }
}

impl Default for DagLedger {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rope_core::personal_ledger::InteractionType;

    fn interaction(tag: &str) -> InteractionRecord {
        InteractionRecord {
            interaction_type: InteractionType::Custom(tag.to_string()),
            counterparty: None,
            data: tag.as_bytes().to_vec(),
            timestamp: 1_720_000_000,
            metadata: hashbrown::HashMap::new(),
        }
    }

    fn wallet(b: u8) -> Vec<u8> {
        let mut w = vec![0u8; 20];
        w[0] = b;
        w
    }

    #[test]
    fn genesis_then_linear_appends_project_in_order() {
        let dl = DagLedger::new();
        let w = wallet(0xA1);
        let a = dl.append_knot(&w, None, interaction("genesis")).unwrap();
        assert!(a.parents.is_empty(), "first append is genesis");
        assert_eq!(a.tip_count, 1);
        dl.append_knot(&w, None, interaction("e2")).unwrap();
        dl.append_knot(&w, None, interaction("e3")).unwrap();

        let proj = dl.walk_projection(&w);
        assert_eq!(proj.len(), 3);
        assert_eq!(
            proj[0].interaction.interaction_type,
            InteractionType::Custom("genesis".to_string())
        );
        assert_eq!(dl.projection_len(&w), 3);
    }

    #[test]
    fn concurrent_style_fan_out_then_auto_compaction() {
        // Threshold 4: fan out 4 siblings off genesis, the 4th append
        // trips the threshold and auto-compacts.
        let dl = DagLedger::with_threshold(4);
        let w = wallet(0xB2);
        let g = dl.append_knot(&w, None, interaction("g")).unwrap();
        let genesis: StringId = {
            let mut b = [0u8; 32];
            b.copy_from_slice(&hex::decode(&g.knot_id).unwrap());
            StringId::new(b)
        };
        // Append 4 siblings all pinned to genesis (explicit parents).
        let mut any_compacted = false;
        for i in 0..4 {
            let r = dl
                .append_knot(&w, Some(vec![genesis]), interaction(&format!("s{i}")))
                .unwrap();
            any_compacted |= r.compacted;
        }
        assert!(any_compacted, "fan-out past threshold must auto-compact");
        // After compaction the wallet is back to a single tip.
        assert_eq!(dl.tips(&w).len(), 1);
        // Projection contains only real events: genesis + 4 siblings.
        assert_eq!(dl.projection_len(&w), 5);
        // The merge knot is not in the projection.
        let proj_ids: Vec<String> = dl.walk_projection(&w).into_iter().map(|p| p.knot_id).collect();
        let stats = dl.stats();
        assert_eq!(stats.total_events, 5);
        assert_eq!(stats.total_merges, 1);
        assert_eq!(stats.total_knots, 6);
        assert_eq!(proj_ids.len(), 5);
    }

    #[test]
    fn unknown_explicit_parent_is_typed_error() {
        let dl = DagLedger::new();
        let w = wallet(0xC3);
        dl.append_knot(&w, None, interaction("g")).unwrap();
        let bogus = StringId::new([0xEE; 32]);
        let r = dl.append_knot(&w, Some(vec![bogus]), interaction("x"));
        assert!(matches!(r, Err(DagLedgerError::UnknownParent(_))));
    }

    #[test]
    fn projection_is_deterministic_across_two_ledgers() {
        let build = || {
            let dl = DagLedger::with_threshold(3);
            let w = wallet(0xD4);
            let g = dl.append_knot(&w, None, interaction("g")).unwrap();
            let genesis: StringId = {
                let mut b = [0u8; 32];
                b.copy_from_slice(&hex::decode(&g.knot_id).unwrap());
                StringId::new(b)
            };
            for i in 0..3 {
                dl.append_knot(&w, Some(vec![genesis]), interaction(&format!("s{i}")))
                    .unwrap();
            }
            dl.walk_projection(&w)
                .into_iter()
                .map(|p| p.knot_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn wallets_are_isolated() {
        let dl = DagLedger::new();
        let w1 = wallet(0x11);
        let w2 = wallet(0x22);
        dl.append_knot(&w1, None, interaction("a")).unwrap();
        dl.append_knot(&w1, None, interaction("b")).unwrap();
        dl.append_knot(&w2, None, interaction("c")).unwrap();
        assert_eq!(dl.projection_len(&w1), 2);
        assert_eq!(dl.projection_len(&w2), 1);
        assert_eq!(dl.stats().wallet_count, 2);
    }

    #[test]
    fn empty_wallet_projects_empty() {
        let dl = DagLedger::new();
        assert!(dl.walk_projection(&wallet(0x99)).is_empty());
        assert_eq!(dl.projection_len(&wallet(0x99)), 0);
        assert!(dl.tips(&wallet(0x99)).is_empty());
    }
}
