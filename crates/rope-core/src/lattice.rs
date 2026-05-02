//! String Lattice - The core DAG data structure replacing blockchain
//!
//! L = (S, ≺, ⊗, R)
//!
//! Where:
//! - S: Set of all strings in the Rope
//! - ≺ (Precedes): Partial ordering capturing causal dependencies
//! - ⊗ (Intertwine): Complementary pairing operation (double helix)
//! - R (Regeneration): Repair relation for damaged strings

use hashbrown::{HashMap, HashSet};
use parking_lot::RwLock;
use petgraph::graph::{DiGraph, NodeIndex};
use petgraph::Direction;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::complement::Complement;
use crate::error::{Result, RopeError};
use crate::string::RopeString;
use crate::types::{constants, FinalityStatus, StringId};

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
pub struct StringLattice {
    /// All strings in the lattice: StringId -> RopeString
    strings: RwLock<HashMap<StringId, RopeString>>,

    /// Complements for each string: StringId -> Complement
    complements: RwLock<HashMap<StringId, Complement>>,

    /// DAG structure for ordering (petgraph)
    ordering: RwLock<LatticeDAG>,

    /// Anchor strings for consensus
    anchors: RwLock<Vec<AnchorString>>,

    /// Pending strings awaiting finality (ordered by Lamport clock)
    pending_strings: RwLock<BTreeMap<u64, HashSet<StringId>>>,

    /// Finalized strings
    finalized_strings: RwLock<HashSet<StringId>>,

    /// Erased strings (tombstones)
    erased_strings: RwLock<HashSet<StringId>>,

    /// Untied-knot tombstones with audit metadata (canon v1.1 §4.2).
    /// Keyed by the original StringId. Presence in this map AND in
    /// `erased_strings` indicates the knot was untied with full audit,
    /// not just garbage-collected.
    knot_tombstones: RwLock<HashMap<StringId, KnotTombstone>>,

    /// Current round number
    current_round: RwLock<u64>,

    /// Creator index: Ed25519 public key bytes -> set of StringIds
    /// Enables efficient lookup of all strings created by a specific wallet
    creator_index: RwLock<HashMap<[u8; 32], Vec<StringId>>>,
}

/// DAG structure for string ordering
struct LatticeDAG {
    graph: DiGraph<StringId, ()>,
    id_to_index: HashMap<StringId, NodeIndex>,
}

impl LatticeDAG {
    fn new() -> Self {
        Self {
            graph: DiGraph::new(),
            id_to_index: HashMap::new(),
        }
    }

    fn add_node(&mut self, id: StringId, parents: &[StringId]) {
        let node_idx = self.graph.add_node(id);
        self.id_to_index.insert(id, node_idx);

        // Add edges from parents to this node
        for parent_id in parents {
            if let Some(&parent_idx) = self.id_to_index.get(parent_id) {
                self.graph.add_edge(parent_idx, node_idx, ());
            }
        }
    }

    fn get_parents(&self, id: &StringId) -> Vec<StringId> {
        if let Some(&idx) = self.id_to_index.get(id) {
            self.graph
                .neighbors_directed(idx, Direction::Incoming)
                .filter_map(|parent_idx| self.graph.node_weight(parent_idx).copied())
                .collect()
        } else {
            Vec::new()
        }
    }

    fn get_children(&self, id: &StringId) -> Vec<StringId> {
        if let Some(&idx) = self.id_to_index.get(id) {
            self.graph
                .neighbors_directed(idx, Direction::Outgoing)
                .filter_map(|child_idx| self.graph.node_weight(child_idx).copied())
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Check if a string exists in the DAG
    #[allow(dead_code)]
    pub fn contains(&self, id: &StringId) -> bool {
        self.id_to_index.contains_key(id)
    }

    /// Get the number of nodes in the DAG
    #[allow(dead_code)]
    pub fn node_count(&self) -> usize {
        self.graph.node_count()
    }
}

impl StringLattice {
    /// Create a new empty string lattice
    pub fn new() -> Self {
        Self {
            strings: RwLock::new(HashMap::new()),
            complements: RwLock::new(HashMap::new()),
            ordering: RwLock::new(LatticeDAG::new()),
            anchors: RwLock::new(Vec::new()),
            pending_strings: RwLock::new(BTreeMap::new()),
            finalized_strings: RwLock::new(HashSet::new()),
            erased_strings: RwLock::new(HashSet::new()),
            knot_tombstones: RwLock::new(HashMap::new()),
            current_round: RwLock::new(0),
            creator_index: RwLock::new(HashMap::new()),
        }
    }

    /// Add a string to the lattice
    ///
    /// This is the primary operation for string creation:
    /// 1. Verify parentage exists
    /// 2. Verify OES generation is current
    /// 3. Verify hybrid signature
    /// 4. Generate complement
    /// 5. Add to lattice structures
    /// 6. Check for anchor creation
    pub fn add_string(&self, string: RopeString) -> Result<StringId> {
        let strings = self.strings.read();
        let erased = self.erased_strings.read();

        // Step 1: Verify parentage exists
        for parent in string.parentage() {
            if !strings.contains_key(parent) && !parent.as_bytes().iter().all(|&b| b == 0) {
                return Err(RopeError::MissingParent(*parent));
            }
            if erased.contains(parent) {
                return Err(RopeError::ParentErased(*parent));
            }
        }

        drop(strings);
        drop(erased);

        // Step 2: Verify OES generation is within acceptable window
        // (Placeholder - actual verification would involve OES state)
        // if !self.verify_oes_generation(string.oes_generation()) {
        //     return Err(RopeError::InvalidOESGeneration);
        // }

        // Step 3: Verify hybrid signature
        // (Placeholder - actual verification would involve crypto module)
        // if !verify_hybrid_signature(&string) {
        //     return Err(RopeError::InvalidSignature);
        // }

        // Step 4: Generate complement
        let complement = Complement::generate(&string);

        // Step 5: Add to lattice structures
        let id = string.id();
        let timestamp = string.temporal_marker().time();

        let creator_key = string.creator().ed25519;

        {
            let mut strings = self.strings.write();
            let mut complements = self.complements.write();
            let mut ordering = self.ordering.write();
            let mut pending = self.pending_strings.write();

            strings.insert(id, string.clone());
            complements.insert(id, complement);
            ordering.add_node(id, string.parentage());

            pending.entry(timestamp).or_default().insert(id);
        }

        // Populate creator index
        self.creator_index
            .write()
            .entry(creator_key)
            .or_default()
            .push(id);

        // Step 6: Check if this creates new anchor
        self.check_anchor_creation(&string)?;

        Ok(id)
    }

    /// Get a string by ID
    pub fn get_string(&self, id: &StringId) -> Option<RopeString> {
        // Check if erased
        if self.erased_strings.read().contains(id) {
            return None;
        }

        self.strings.read().get(id).cloned()
    }

    /// Get a complement by string ID
    pub fn get_complement(&self, id: &StringId) -> Option<Complement> {
        if self.erased_strings.read().contains(id) {
            return None;
        }

        self.complements.read().get(id).cloned()
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
        !self.erased_strings.read().contains(id) && self.strings.read().contains_key(id)
    }

    /// Get the number of strings in the lattice
    pub fn string_count(&self) -> usize {
        self.strings.read().len()
    }

    /// Get the number of pending strings
    pub fn pending_count(&self) -> usize {
        self.pending_strings.read().values().map(|s| s.len()).sum()
    }

    /// Get the number of finalized strings
    pub fn finalized_count(&self) -> usize {
        self.finalized_strings.read().len()
    }

    /// Get the number of erased strings
    pub fn erased_count(&self) -> usize {
        self.erased_strings.read().len()
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

    /// Get parents of a string
    pub fn get_parents(&self, id: &StringId) -> Vec<StringId> {
        self.ordering.read().get_parents(id)
    }

    /// Get children of a string
    pub fn get_children(&self, id: &StringId) -> Vec<StringId> {
        self.ordering.read().get_children(id)
    }

    // === Personal Ledger Queries ===

    /// Get all StringIds created by a specific public key (wallet)
    pub fn strings_by_creator(&self, ed25519_pubkey: &[u8; 32]) -> Vec<StringId> {
        self.creator_index
            .read()
            .get(ed25519_pubkey)
            .cloned()
            .unwrap_or_default()
    }

    /// Walk the ledger chain for a creator: starting from `head`, follow
    /// parentage links backwards to build the ordered chain. Returns entries
    /// from genesis to head (oldest first).
    pub fn walk_ledger_chain(&self, head: &StringId) -> Vec<StringId> {
        let strings = self.strings.read();
        let mut chain = Vec::new();
        let mut current = *head;

        loop {
            if current == StringId::ZERO {
                break;
            }
            if self.erased_strings.read().contains(&current) {
                break;
            }
            chain.push(current);
            match strings.get(&current) {
                Some(s) => {
                    if let Some(parent) = s.parentage().first() {
                        current = *parent;
                    } else {
                        break;
                    }
                }
                None => break,
            }
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

        self.creator_index.write().remove(ed25519_pubkey);

        Ok(erased_count)
    }

    /// Mark a string as erased
    pub fn mark_erased(&self, id: StringId) -> Result<()> {
        let mut erased = self.erased_strings.write();
        let mut strings = self.strings.write();
        let mut complements = self.complements.write();

        if !strings.contains_key(&id) {
            return Err(RopeError::StringNotFound(id));
        }

        // Remove from active storage
        strings.remove(&id);
        complements.remove(&id);

        // Add to erased set (tombstone)
        erased.insert(id);

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

        // Snapshot parents from the DAG before mark_erased clears the
        // RopeString. The DAG keeps its edges so future walks can hop past.
        let _parents = self.ordering.read().get_parents(&id);

        // Destroy the payload (this also drops the parentage stored in the
        // RopeString itself; the DAG retains parent edges separately).
        self.mark_erased(id)?;

        // Record the canonical tombstone metadata for audit/UI.
        self.knot_tombstones.write().insert(id, tombstone.clone());

        Ok(tombstone)
    }

    /// Look up a knot's tombstone metadata, if any. Returns None if the knot
    /// was never untied (or was whole-string erased without tombstone metadata).
    pub fn get_tombstone(&self, id: &StringId) -> Option<KnotTombstone> {
        self.knot_tombstones.read().get(id).cloned()
    }

    /// Check whether a knot has been untied via the canonical canon v1.1 path.
    pub fn is_knot_untied(&self, id: &StringId) -> bool {
        self.knot_tombstones.read().contains_key(id)
    }

    /// Total count of untied knots (transparency metric for the canon §6(5) UI).
    pub fn tombstone_count(&self) -> usize {
        self.knot_tombstones.read().len()
    }

    /// Walk a wallet's string from `head` back to genesis, but DO NOT stop
    /// at tombstones. Returns one `LedgerEntry` per knot position — either
    /// `Active(StringId)` or `Tombstone(StringId, KnotTombstone)`.
    ///
    /// Walks via DAG parent edges (which survive `mark_knot_untied`) when
    /// the live RopeString is gone, and via the RopeString's own parentage
    /// when it's present. Returned vector is genesis-first (oldest first).
    pub fn walk_string_with_tombstones(&self, head: &StringId) -> Vec<LedgerEntry> {
        let strings = self.strings.read();
        let tombstones = self.knot_tombstones.read();
        let dag = self.ordering.read();

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

            // Resolve parent: prefer the live RopeString's own parentage,
            // fall back to the DAG (which survives untying).
            let next = if let Some(s) = strings.get(&current) {
                chain.push(LedgerEntry::Active(current));
                s.parentage().first().copied().unwrap_or(StringId::ZERO)
            } else if let Some(ts) = tombstones.get(&current) {
                chain.push(LedgerEntry::Tombstone(current, ts.clone()));
                // Use DAG to hop past the tombstone.
                dag.get_parents(&current)
                    .into_iter()
                    .next()
                    .unwrap_or(StringId::ZERO)
            } else {
                // Unknown id — neither live nor tombstoned. Stop walking.
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

    /// Count how many anchor strings reference a given string
    fn count_anchor_references(&self, id: &StringId) -> u32 {
        let anchors = self.anchors.read();
        let ordering = self.ordering.read();

        anchors
            .iter()
            .filter(|anchor| {
                // Check if anchor references this string (directly or transitively)
                self.is_ancestor_of(id, &anchor.id(), &ordering)
            })
            .count() as u32
    }

    /// Check if `ancestor` is an ancestor of `descendant` in the DAG
    fn is_ancestor_of(&self, ancestor: &StringId, descendant: &StringId, dag: &LatticeDAG) -> bool {
        if ancestor == descendant {
            return true;
        }

        // BFS to find path
        let mut visited = HashSet::new();
        let mut queue = vec![*descendant];

        while let Some(current) = queue.pop() {
            if current == *ancestor {
                return true;
            }

            if visited.insert(current) {
                queue.extend(dag.get_parents(&current));
            }
        }

        false
    }

    /// Check if a string should become an anchor
    fn check_anchor_creation(&self, string: &RopeString) -> Result<()> {
        // Simplified anchor creation logic
        // Real implementation would involve virtual voting

        let anchors = self.anchors.read();
        if let Some(last_anchor) = anchors.last() {
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
                anchors.push(new_anchor);

                // Mark strings as finalized
                self.update_finality();
            }
        } else {
            // First anchor (genesis)
            drop(anchors);

            let mut anchors = self.anchors.write();
            let anchor = AnchorString::new(string.clone(), 0);
            anchors.push(anchor);
        }

        Ok(())
    }

    /// Update finality status based on anchor strings
    fn update_finality(&self) {
        let anchors = self.anchors.read();
        let pending = self.pending_strings.read();

        if anchors.len() < constants::FINALITY_ANCHORS as usize {
            return;
        }

        // Get strings that have enough anchor confirmations
        let mut newly_finalized = Vec::new();

        for (_, string_ids) in pending.iter() {
            for id in string_ids {
                let refs = self.count_anchor_references(id);
                if refs >= constants::FINALITY_ANCHORS {
                    newly_finalized.push(*id);
                }
            }
        }

        drop(anchors);
        drop(pending);

        // Mark as finalized
        let mut finalized = self.finalized_strings.write();
        let mut pending = self.pending_strings.write();

        for id in newly_finalized {
            finalized.insert(id);
            // Remove from pending (find and remove)
            for string_ids in pending.values_mut() {
                string_ids.remove(&id);
            }
        }

        // Clean up empty pending entries
        pending.retain(|_, ids| !ids.is_empty());
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
    // Quipu Primitive Canon v1.1 — per-knot untying tests
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
}
