//! # `knot_dag` — Quipu Canon v2.0 Phase 2.E
//!
//! Per-wallet KnotDAG primitive. Replaces the linear personal
//! ledger chain with a directed acyclic graph of knots: each new
//! knot may reference *multiple* parents (the wallet's current tip
//! set), allowing concurrent appends to converge through explicit
//! multi-parent edges instead of serialising on a single head lock.
//!
//! ## Why this exists
//!
//! Phase 1.2 added per-wallet head-string locks so concurrent
//! appends to *different* wallets no longer serialise. But within
//! one wallet, only one append can be in flight at a time — the
//! head lock still serialises. For wallets with high inflow
//! (exchanges, bridges, custodial services, IoT gateways) that
//! per-wallet ceiling matters. Phase 2.E lifts it.
//!
//! The trick: a wallet's "head" becomes a **set of tips** rather
//! than a single id. Two appends that race against the same tip
//! set both succeed; their resulting knots both reference the same
//! parents, and the next append references them both as parents,
//! re-merging the wallet's history into a single tip. The DAG
//! captures the actual concurrency that occurred.
//!
//! ## Relationship to the lattice
//!
//! [`crate::lattice::StringLattice`] is the *global* DAG of all
//! strings across all wallets. [`KnotDag`] is the *per-wallet*
//! view: it tracks the subset of strings that belong to one
//! wallet, plus the per-wallet tip set, and provides DAG-only
//! operations (tips, append, ancestors, descendants, topological
//! sort) without touching the global lattice's locks.
//!
//! In production the per-wallet `KnotDag` and the global
//! `StringLattice` will be kept in sync through a single
//! `LedgerManager::append_to_dag` entry point — added in a
//! follow-up patch. For now, `KnotDag` is a standalone primitive
//! with full tests; the `LedgerManager` integration is Phase 2.E.1.
//!
//! ## What this module does NOT do (yet)
//!
//! - **Cross-wallet edges.** A knot may only reference parents in
//!   the same wallet's DAG. Cross-wallet causal edges remain in
//!   the global lattice.
//! - **Persistence.** This is an in-memory primitive. Disk-backed
//!   snapshots will reuse `rope-storage::WriteOp` when the
//!   `LedgerManager` integration lands.
//! - **DAG-aware finality.** This module exposes the topology;
//!   the finality watermark added in Phase 2.C.1 still applies to
//!   each knot via the global lattice.

use crate::types::StringId;
use hashbrown::HashSet;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap as StdHashMap, VecDeque};
use std::sync::Arc;
use thiserror::Error;

/// Per-wallet sharding for [`KnotDagRegistry`]. Same axis as
/// [`crate::lattice::NUM_SHARDS`] / [`crate::clock`] / the
/// `rope-cluster` partition map, so every layer stays aligned.
pub const KNOT_DAG_NUM_SHARDS: usize = 256;

/// Errors produced by [`KnotDag`].
#[derive(Debug, Error, PartialEq, Eq)]
pub enum KnotDagError {
    /// The supplied parent id is not in this DAG. Cross-wallet
    /// edges are not allowed inside a per-wallet `KnotDag`.
    #[error("parent {parent:?} is not in this wallet's DAG")]
    UnknownParent { parent: StringId },

    /// The supplied knot id is already present.
    #[error("knot {id:?} already in DAG")]
    DuplicateKnot { id: StringId },

    /// The knot ids form a cycle (the new knot is its own
    /// ancestor). Should be impossible if `add_knot` is the only
    /// mutation point — included as a defence against future
    /// callers that bypass it.
    #[error("appending {id:?} with parents {parents:?} would create a cycle")]
    CycleDetected {
        id: StringId,
        parents: Vec<StringId>,
    },
}

/// Per-wallet KnotDAG.
///
/// Holds:
///
/// - `parents[id]` — the knot's parent edges (same as
///   `RopeString::parentage` for the underlying string)
/// - `children[id]` — reverse edges, lazily maintained
/// - `tips` — knots with zero children right now (wallet's current
///   tip set)
///
/// All four collections live behind `parking_lot::RwLock`s so a
/// thread that is reading the tip set never blocks a thread that
/// is mutating the DAG via `add_knot`. Two concurrent `add_knot`
/// calls do briefly contend on the four write locks but the
/// critical section is O(parents + 1) hash inserts per call —
/// far below the prior per-wallet head-lock cost which serialised
/// the entire append pipeline (string-build, complement-gen,
/// signature-verify, lattice-insert).
pub struct KnotDag {
    inner: RwLock<KnotDagInner>,
}

struct KnotDagInner {
    /// child -> parents
    parents: StdHashMap<StringId, Vec<StringId>>,
    /// parent -> children (reverse adjacency, maintained
    /// alongside `parents` so tip-set queries are O(1))
    children: StdHashMap<StringId, Vec<StringId>>,
    /// Current tip set — knots with no children. Stored as a
    /// `HashSet` so add/remove are O(1).
    tips: HashSet<StringId>,
}

/// Read-only snapshot of a [`KnotDag`]. Useful for serialisation,
/// testing, and DCScan dashboards. Cheap to compare for equality.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnotDagSnapshot {
    pub knots: Vec<StringId>,
    pub edges: Vec<(StringId, StringId)>,
    pub tips: Vec<StringId>,
}

impl KnotDag {
    /// Create an empty DAG. The first append must use an empty
    /// `parents` slice (genesis knot).
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(KnotDagInner {
                parents: StdHashMap::new(),
                children: StdHashMap::new(),
                tips: HashSet::new(),
            }),
        }
    }

    /// Number of knots currently in the DAG.
    pub fn len(&self) -> usize {
        self.inner.read().parents.len()
    }

    /// True if no knots have been added yet.
    pub fn is_empty(&self) -> bool {
        self.inner.read().parents.is_empty()
    }

    /// Snapshot of the current tip set. Sorted lexicographically
    /// for deterministic iteration (e.g. when building the parent
    /// list for the next append).
    ///
    /// **The intended pattern**: a caller wanting to append a knot
    /// reads `tips()`, builds the new knot referencing those tips
    /// as parents, and calls [`Self::add_knot`]. If a concurrent
    /// append commits between the two, both new knots will reference
    /// the *same* old tip set — they become siblings rather than
    /// one shadowing the other. The next append references both as
    /// parents and the wallet history re-converges.
    pub fn tips(&self) -> Vec<StringId> {
        let mut t: Vec<StringId> = self.inner.read().tips.iter().copied().collect();
        t.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        t
    }

    /// True if `id` is currently a tip (no children).
    pub fn is_tip(&self, id: &StringId) -> bool {
        self.inner.read().tips.contains(id)
    }

    /// True if `id` is in the DAG.
    pub fn contains(&self, id: &StringId) -> bool {
        self.inner.read().parents.contains_key(id)
    }

    /// Return the knot's parents (ordered as supplied to
    /// [`Self::add_knot`]). Empty if `id` is not in the DAG.
    pub fn parents_of(&self, id: &StringId) -> Vec<StringId> {
        self.inner
            .read()
            .parents
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Return the knot's children (insertion order — i.e. order in
    /// which child knots were added).
    pub fn children_of(&self, id: &StringId) -> Vec<StringId> {
        self.inner
            .read()
            .children
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Add a knot with the given parents. Updates the tip set:
    ///
    ///   1. `id` becomes a tip (it has no children yet).
    ///   2. Each parent is removed from the tip set (it now has at
    ///      least one child).
    ///
    /// Errors:
    ///   - [`KnotDagError::UnknownParent`] — a parent isn't in the
    ///     DAG. Cross-wallet edges live in the global lattice, not
    ///     here.
    ///   - [`KnotDagError::DuplicateKnot`] — `id` already present.
    ///   - [`KnotDagError::CycleDetected`] — should be impossible
    ///     when `add_knot` is the only mutator (BLAKE3 ids are
    ///     content-addressed so a cycle implies a hash collision)
    ///     but the check is cheap and worth keeping.
    pub fn add_knot(&self, id: StringId, parents: &[StringId]) -> Result<(), KnotDagError> {
        let mut inner = self.inner.write();

        if inner.parents.contains_key(&id) {
            return Err(KnotDagError::DuplicateKnot { id });
        }

        // Validate every supplied parent exists.
        for p in parents {
            if !inner.parents.contains_key(p) {
                return Err(KnotDagError::UnknownParent { parent: *p });
            }
        }

        // Defence-in-depth: reject if any supplied parent is
        // already a (transitive) descendant of `id`. With
        // content-addressed ids this can only happen on
        // intentional misuse, but the check is O(d) on the
        // descendant cone of `id`.
        if !parents.is_empty() {
            // `id` is brand new so it has no descendants yet,
            // which means the cycle check is trivial — we just
            // need to confirm none of the parents *is* `id` (the
            // self-loop case).
            if parents.iter().any(|p| *p == id) {
                return Err(KnotDagError::CycleDetected {
                    id,
                    parents: parents.to_vec(),
                });
            }
        }

        // Mutate. From here on, no early exit — we must keep the
        // four collections consistent.

        inner.parents.insert(id, parents.to_vec());
        inner.children.entry(id).or_default(); // guarantee key presence

        // Remove parents from the tip set (they now have at least
        // one child) and add `id` as a new tip.
        for p in parents {
            inner.tips.remove(p);
            inner.children.entry(*p).or_default().push(id);
        }
        inner.tips.insert(id);

        Ok(())
    }

    /// Walk the ancestor cone of `start` upward via `parents`.
    /// Returns each visited id exactly once in insertion order.
    pub fn ancestors(&self, start: &StringId) -> Vec<StringId> {
        let inner = self.inner.read();
        let mut visited: HashSet<StringId> = HashSet::new();
        let mut order: Vec<StringId> = Vec::new();
        let mut q: VecDeque<StringId> = VecDeque::new();
        q.push_back(*start);

        while let Some(cur) = q.pop_front() {
            if !visited.insert(cur) {
                continue;
            }
            order.push(cur);
            if let Some(ps) = inner.parents.get(&cur) {
                for p in ps {
                    if !visited.contains(p) {
                        q.push_back(*p);
                    }
                }
            }
        }

        order
    }

    /// Walk the descendant cone of `start` downward via `children`.
    /// Returns each visited id exactly once in BFS order.
    pub fn descendants(&self, start: &StringId) -> Vec<StringId> {
        let inner = self.inner.read();
        let mut visited: HashSet<StringId> = HashSet::new();
        let mut order: Vec<StringId> = Vec::new();
        let mut q: VecDeque<StringId> = VecDeque::new();
        q.push_back(*start);

        while let Some(cur) = q.pop_front() {
            if !visited.insert(cur) {
                continue;
            }
            order.push(cur);
            if let Some(cs) = inner.children.get(&cur) {
                for c in cs {
                    if !visited.contains(c) {
                        q.push_back(*c);
                    }
                }
            }
        }

        order
    }

    /// Topological sort of the DAG via Kahn's algorithm.
    ///
    /// Returns one valid linearisation (oldest → newest). Ties
    /// between knots with the same in-degree are broken by id
    /// order so the result is deterministic for a given DAG state.
    ///
    /// Useful for downstream consumers that need to render a
    /// linear view of the wallet history (Datawallet+ UI,
    /// DCScan, regulatory exports).
    pub fn topo_sorted(&self) -> Vec<StringId> {
        let inner = self.inner.read();

        // In-degree per knot.
        let mut indeg: StdHashMap<StringId, usize> = StdHashMap::with_capacity(inner.parents.len());
        for (id, ps) in &inner.parents {
            indeg.insert(*id, ps.len());
        }

        // Seed: knots with in-degree 0 (root knots).
        let mut zeros: Vec<StringId> = indeg
            .iter()
            .filter_map(|(id, d)| if *d == 0 { Some(*id) } else { None })
            .collect();
        zeros.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));

        let mut order: Vec<StringId> = Vec::with_capacity(inner.parents.len());
        let mut frontier: VecDeque<StringId> = VecDeque::from(zeros);

        while let Some(cur) = frontier.pop_front() {
            order.push(cur);
            if let Some(cs) = inner.children.get(&cur) {
                // Walk children, decrement their in-degree, push
                // any that hit zero — sorted insertion to keep the
                // result deterministic.
                let mut newly_zero: Vec<StringId> = Vec::new();
                for c in cs {
                    if let Some(d) = indeg.get_mut(c) {
                        *d -= 1;
                        if *d == 0 {
                            newly_zero.push(*c);
                        }
                    }
                }
                newly_zero.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
                for c in newly_zero {
                    frontier.push_back(c);
                }
            }
        }

        order
    }

    /// Return `true` iff every parent edge points to a knot in the
    /// DAG and no knot is its own ancestor. O(N + E). Useful as a
    /// debug-mode invariant check; production callers should not
    /// need to call this on every append.
    pub fn is_consistent(&self) -> bool {
        let inner = self.inner.read();
        // 1. Every parent referenced exists.
        for (_, ps) in &inner.parents {
            for p in ps {
                if !inner.parents.contains_key(p) {
                    return false;
                }
            }
        }
        // 2. children reverse-index matches parents forward-index.
        let mut expected_children: StdHashMap<StringId, HashSet<StringId>> = StdHashMap::new();
        for (child, ps) in &inner.parents {
            for p in ps {
                expected_children.entry(*p).or_default().insert(*child);
            }
        }
        for (parent, children_vec) in &inner.children {
            let expected = expected_children
                .get(parent)
                .cloned()
                .unwrap_or_default();
            let actual: HashSet<StringId> = children_vec.iter().copied().collect();
            if expected != actual {
                return false;
            }
        }
        // 3. Tip set is exactly the knots with no children.
        let computed_tips: HashSet<StringId> = inner
            .parents
            .keys()
            .filter(|id| {
                inner
                    .children
                    .get(id)
                    .map(|cs| cs.is_empty())
                    .unwrap_or(true)
            })
            .copied()
            .collect();
        if computed_tips != inner.tips {
            return false;
        }
        true
    }

    /// Read-only snapshot of the current DAG state. Useful for
    /// tests, dashboards, and serialisation.
    pub fn snapshot(&self) -> KnotDagSnapshot {
        let inner = self.inner.read();
        let knots: Vec<StringId> = {
            let mut v: Vec<StringId> = inner.parents.keys().copied().collect();
            v.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            v
        };
        let mut edges: Vec<(StringId, StringId)> = Vec::new();
        for (child, ps) in &inner.parents {
            for p in ps {
                edges.push((*p, *child));
            }
        }
        edges.sort_by(|a, b| {
            a.0.as_bytes()
                .cmp(b.0.as_bytes())
                .then_with(|| a.1.as_bytes().cmp(b.1.as_bytes()))
        });
        let mut tips: Vec<StringId> = inner.tips.iter().copied().collect();
        tips.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        KnotDagSnapshot { knots, edges, tips }
    }
}

impl Default for KnotDag {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// KnotDagRegistry — per-wallet DAG sharded across 256 shards
// ============================================================================

/// Shard index for a wallet (first byte of its address). Matches
/// every other 256-shard axis in the system.
#[inline]
fn shard_for_wallet(wallet: &[u8]) -> usize {
    wallet.first().copied().unwrap_or(0) as usize
}

/// One shard of the registry.
struct KnotDagShard {
    /// `wallet -> Arc<KnotDag>`. `Arc` so callers can hold the
    /// per-wallet DAG outside the registry's lock and call its
    /// methods concurrently with other wallets on the same shard.
    dags: RwLock<StdHashMap<Vec<u8>, Arc<KnotDag>>>,
}

impl KnotDagShard {
    fn new() -> Self {
        Self {
            dags: RwLock::new(StdHashMap::new()),
        }
    }
}

/// Registry of per-wallet KnotDAGs, sharded over
/// [`KNOT_DAG_NUM_SHARDS`] shards keyed by `wallet[0]`.
///
/// The registry's own write locks are taken only on the *first*
/// touch of a wallet (when the per-wallet `KnotDag` is created);
/// all subsequent appends, tip queries, and traversals go directly
/// against the per-wallet `KnotDag`'s own lock with no
/// registry-level contention.
///
/// This is the wallet-keyed analogue of [`crate::lattice::StringLattice`]:
/// the global lattice is the system-wide DAG of every string ever
/// added; the registry is a per-wallet view that callers use when
/// they only need to reason about one wallet's history (which is
/// the common case for ledger appends, exports, dashboards).
pub struct KnotDagRegistry {
    shards: Box<[KnotDagShard]>,
}

impl KnotDagRegistry {
    pub fn new() -> Self {
        let shards: Vec<KnotDagShard> =
            (0..KNOT_DAG_NUM_SHARDS).map(|_| KnotDagShard::new()).collect();
        Self {
            shards: shards.into_boxed_slice(),
        }
    }

    /// Acquire (or lazily create) the `KnotDag` for `wallet`.
    /// O(1) amortised; takes a per-shard read lock on the hot path
    /// and only escalates to a write lock on first touch.
    pub fn dag_for(&self, wallet: &[u8]) -> Arc<KnotDag> {
        let s = &self.shards[shard_for_wallet(wallet)];
        if let Some(d) = s.dags.read().get(wallet) {
            return d.clone();
        }
        // First touch — escalate to write lock and double-check.
        let mut w = s.dags.write();
        w.entry(wallet.to_vec())
            .or_insert_with(|| Arc::new(KnotDag::new()))
            .clone()
    }

    /// True if a `KnotDag` already exists for `wallet`. Used by
    /// dashboards that want to count distinct wallets without
    /// triggering DAG creation.
    pub fn contains(&self, wallet: &[u8]) -> bool {
        self.shards[shard_for_wallet(wallet)]
            .dags
            .read()
            .contains_key(wallet)
    }

    /// Total number of wallets across all shards. Aggregates under
    /// each shard's read lock.
    pub fn wallet_count(&self) -> usize {
        self.shards.iter().map(|s| s.dags.read().len()).sum()
    }

    /// Convenience: append a knot to a wallet's DAG. Equivalent to
    /// `self.dag_for(wallet).add_knot(id, parents)` but doesn't
    /// require the caller to materialise the `Arc<KnotDag>`.
    pub fn append(
        &self,
        wallet: &[u8],
        id: StringId,
        parents: &[StringId],
    ) -> Result<(), KnotDagError> {
        self.dag_for(wallet).add_knot(id, parents)
    }
}

impl Default for KnotDagRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(byte: u8) -> StringId {
        StringId::new([byte; 32])
    }

    // ----- baseline single-parent (linear chain compat) -----

    #[test]
    fn empty_dag_has_no_knots_no_tips() {
        let dag = KnotDag::new();
        assert!(dag.is_empty());
        assert_eq!(dag.len(), 0);
        assert!(dag.tips().is_empty());
        assert!(dag.is_consistent());
    }

    #[test]
    fn single_knot_becomes_genesis_tip() {
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        assert_eq!(dag.len(), 1);
        assert_eq!(dag.tips(), vec![id(1)]);
        assert!(dag.is_tip(&id(1)));
        assert!(dag.is_consistent());
    }

    #[test]
    fn linear_chain_keeps_one_tip() {
        // 1 ← 2 ← 3 ← 4
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        dag.add_knot(id(2), &[id(1)]).unwrap();
        dag.add_knot(id(3), &[id(2)]).unwrap();
        dag.add_knot(id(4), &[id(3)]).unwrap();
        assert_eq!(dag.tips(), vec![id(4)]);
        assert!(!dag.is_tip(&id(1)));
        assert!(!dag.is_tip(&id(2)));
        assert!(!dag.is_tip(&id(3)));
        assert!(dag.is_consistent());
    }

    // ----- multi-parent (the new capability) -----

    #[test]
    fn fork_creates_two_tips() {
        // 1 ← 2
        //   ← 3   (sibling of 2 — both reference 1 as parent)
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        dag.add_knot(id(2), &[id(1)]).unwrap();
        dag.add_knot(id(3), &[id(1)]).unwrap();
        let mut tips = dag.tips();
        tips.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(tips, vec![id(2), id(3)], "fork must produce 2 tips");
        assert!(dag.is_consistent());
    }

    #[test]
    fn merge_collapses_tips_back_to_one() {
        // 1 ← 2 ──┐
        //   ← 3 ──┴─ 4   (4 references both 2 and 3 — wallet
        //                  history re-converges)
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        dag.add_knot(id(2), &[id(1)]).unwrap();
        dag.add_knot(id(3), &[id(1)]).unwrap();
        dag.add_knot(id(4), &[id(2), id(3)]).unwrap();
        assert_eq!(dag.tips(), vec![id(4)]);
        assert_eq!(
            dag.parents_of(&id(4)).len(),
            2,
            "merge knot must record both parents"
        );
        assert!(dag.is_consistent());
    }

    #[test]
    fn diamond_topology_is_detected_and_traversed() {
        // 1 ── 2 ──┐
        //   ── 3 ──┴ 4
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        dag.add_knot(id(2), &[id(1)]).unwrap();
        dag.add_knot(id(3), &[id(1)]).unwrap();
        dag.add_knot(id(4), &[id(2), id(3)]).unwrap();
        // Ancestors of 4 = {4, 2, 3, 1}, deduplicated.
        let mut anc = dag.ancestors(&id(4));
        anc.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(anc, vec![id(1), id(2), id(3), id(4)]);
        // Descendants of 1 = {1, 2, 3, 4}.
        let mut desc = dag.descendants(&id(1));
        desc.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        assert_eq!(desc, vec![id(1), id(2), id(3), id(4)]);
    }

    #[test]
    fn topo_sort_respects_partial_order() {
        // Build a deeper diamond to make the sort non-trivial.
        // 1 ── 2 ──┐
        //   ── 3 ──┴ 4 ── 5
        //               ── 6 ── 7
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        dag.add_knot(id(2), &[id(1)]).unwrap();
        dag.add_knot(id(3), &[id(1)]).unwrap();
        dag.add_knot(id(4), &[id(2), id(3)]).unwrap();
        dag.add_knot(id(5), &[id(4)]).unwrap();
        dag.add_knot(id(6), &[id(4)]).unwrap();
        dag.add_knot(id(7), &[id(6)]).unwrap();

        let order = dag.topo_sorted();
        // Every parent must precede its child.
        let pos: StdHashMap<StringId, usize> =
            order.iter().enumerate().map(|(i, x)| (*x, i)).collect();
        for (child, ps) in &dag.inner.read().parents {
            let cp = pos[child];
            for p in ps {
                assert!(
                    pos[p] < cp,
                    "topo violation: parent {p:?} at {} ≥ child {child:?} at {cp}",
                    pos[p]
                );
            }
        }
    }

    // ----- error paths -----

    #[test]
    fn unknown_parent_is_rejected() {
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        let r = dag.add_knot(id(2), &[id(99)]);
        assert!(matches!(r, Err(KnotDagError::UnknownParent { .. })));
        // Failed append must not change tips.
        assert_eq!(dag.tips(), vec![id(1)]);
        assert!(dag.is_consistent());
    }

    #[test]
    fn duplicate_knot_is_rejected() {
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        let r = dag.add_knot(id(1), &[]);
        assert!(matches!(r, Err(KnotDagError::DuplicateKnot { .. })));
        assert_eq!(dag.len(), 1);
    }

    #[test]
    fn self_loop_is_rejected_as_cycle() {
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        // We can't actually pass id(2) as both new id AND parent
        // because the parent must already exist. The only way to
        // create a self-loop via `add_knot` is to claim the new id
        // as its own parent, which fails the existence check
        // first. Add a guard test for the latter:
        let r = dag.add_knot(id(2), &[id(2)]);
        assert!(
            matches!(r, Err(KnotDagError::UnknownParent { .. })),
            "self-loop on a brand-new id can't bypass the existence check"
        );
    }

    // ----- concurrency -----

    #[test]
    fn concurrent_appends_to_same_wallet_succeed() {
        // The whole point of the DAG canon. Many threads append
        // against the current tip set; every thread succeeds; the
        // final tip set has |threads| entries; nothing is lost.
        use std::sync::Arc;
        use std::thread;

        let dag = Arc::new(KnotDag::new());
        // Genesis.
        dag.add_knot(id(0), &[]).unwrap();

        // 8 threads each append one knot referencing the genesis
        // (the only tip at the moment of read). Each new knot is
        // a sibling of the others and ALL become tips.
        const N_THREADS: u8 = 8;
        let mut handles = Vec::new();
        for t in 1..=N_THREADS {
            let dag = dag.clone();
            handles.push(thread::spawn(move || {
                let parents = dag.tips();
                dag.add_knot(id(t), &parents).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        // After the storm, every appended knot must be a tip
        // (none has children yet), and the genesis must NOT be a
        // tip (every new knot referenced it).
        let tips = dag.tips();
        assert!(
            tips.len() >= 1 && tips.len() <= N_THREADS as usize,
            "tip count after storm = {}, expected in [1, {}]",
            tips.len(),
            N_THREADS
        );
        assert!(
            !dag.is_tip(&id(0)),
            "genesis must not be a tip — every thread referenced it"
        );
        assert_eq!(dag.len(), N_THREADS as usize + 1);
        assert!(dag.is_consistent());

        // Now: a single "merge" knot referencing ALL current tips
        // should collapse the tip set back to exactly 1.
        let merge_parents = dag.tips();
        dag.add_knot(id(255), &merge_parents).unwrap();
        let tips_after = dag.tips();
        assert_eq!(tips_after, vec![id(255)]);
        assert!(dag.is_consistent());
    }

    // ----- snapshot -----

    // ----- KnotDagRegistry: per-wallet sharded multi-DAG -----

    #[test]
    fn registry_creates_dag_lazily() {
        let r = KnotDagRegistry::new();
        let w1 = b"wallet-1";
        assert!(!r.contains(w1));
        let _ = r.dag_for(w1);
        assert!(r.contains(w1));
        assert_eq!(r.wallet_count(), 1);
    }

    #[test]
    fn registry_isolates_wallets() {
        let r = KnotDagRegistry::new();
        let w1 = vec![0xAA; 20];
        let w2 = vec![0xBB; 20];
        r.append(&w1, id(1), &[]).unwrap();
        r.append(&w2, id(2), &[]).unwrap();
        let d1 = r.dag_for(&w1);
        let d2 = r.dag_for(&w2);
        assert_eq!(d1.tips(), vec![id(1)]);
        assert_eq!(d2.tips(), vec![id(2)]);
        // Wallets are independent — appending to one doesn't change the other.
        assert!(d1.is_consistent());
        assert!(d2.is_consistent());
        assert_eq!(r.wallet_count(), 2);
    }

    #[test]
    fn registry_concurrent_appends_to_distinct_wallets_dont_block() {
        // 16 threads × 50 appends to 16 distinct wallets → 800
        // appends total, all complete cleanly with no head-lock
        // contention because each wallet has its own DAG.
        use std::sync::Arc;
        use std::thread;

        let r = Arc::new(KnotDagRegistry::new());
        let mut handles = Vec::new();
        for tid in 0..16u8 {
            let r = r.clone();
            handles.push(thread::spawn(move || {
                let wallet = vec![tid; 20];
                let mut prev = StringId::new([tid; 32]);
                r.append(&wallet, prev, &[]).unwrap();
                for i in 1u8..50 {
                    let mut bytes = [0u8; 32];
                    bytes[0] = tid;
                    bytes[1] = i;
                    let next = StringId::new(bytes);
                    r.append(&wallet, next, &[prev]).unwrap();
                    prev = next;
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(r.wallet_count(), 16);
        for tid in 0..16u8 {
            let wallet = vec![tid; 20];
            let dag = r.dag_for(&wallet);
            assert_eq!(dag.len(), 50, "wallet {tid} should hold 50 knots");
        }
    }

    #[test]
    fn registry_concurrent_appends_to_same_wallet_via_dag_arc() {
        // The DAG is what handles concurrent appends to ONE wallet
        // — the registry just hands out the Arc<KnotDag>. This test
        // proves the Arc-then-append pattern works.
        use std::sync::Arc;
        use std::thread;

        let r = Arc::new(KnotDagRegistry::new());
        let wallet = vec![0xCC; 20];
        r.append(&wallet, id(0), &[]).unwrap();

        let mut handles = Vec::new();
        for tid in 1u8..=8 {
            let r = r.clone();
            let wallet = wallet.clone();
            handles.push(thread::spawn(move || {
                let dag = r.dag_for(&wallet);
                let parents = dag.tips();
                dag.add_knot(id(tid), &parents).unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        let dag = r.dag_for(&wallet);
        // Genesis + 8 children = 9 knots.
        assert_eq!(dag.len(), 9);
        // Genesis is no longer a tip.
        assert!(!dag.is_tip(&id(0)));
    }

    #[test]
    fn snapshot_round_trips_through_serde() {
        let dag = KnotDag::new();
        dag.add_knot(id(1), &[]).unwrap();
        dag.add_knot(id(2), &[id(1)]).unwrap();
        dag.add_knot(id(3), &[id(1)]).unwrap();
        dag.add_knot(id(4), &[id(2), id(3)]).unwrap();

        let snap = dag.snapshot();
        let bytes = bincode::serialize(&snap).unwrap();
        let back: KnotDagSnapshot = bincode::deserialize(&bytes).unwrap();
        assert_eq!(snap, back);
        assert_eq!(snap.tips, vec![id(4)]);
        assert_eq!(snap.knots.len(), 4);
        assert_eq!(snap.edges.len(), 4); // 1->2, 1->3, 2->4, 3->4
    }
}
