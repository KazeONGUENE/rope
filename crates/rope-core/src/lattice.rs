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

/// String Lattice - The core data structure of Datachain Rope
///
/// Replaces blockchain's linear chain with a multi-dimensional lattice
/// of intertwined strings that can be added, verified, and erased.
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

    /// Current round number
    current_round: RwLock<u64>,
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
            current_round: RwLock::new(0),
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
}
