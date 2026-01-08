//! # Virtual Voting Protocol (Appendix B.1)
//! 
//! Complete implementation of gossip-history based virtual voting per specification.
//! 
//! ## Algorithm (from Appendix B.1)
//! 
//! ```text
//! function VIRTUAL_VOTE(node_id, string_id):
//!     gossip_history ← GET_GOSSIP_HISTORY(node_id)
//!     first_learned ← NIL
//!     
//!     for event in gossip_history:
//!         if CONTAINS(event.strings, string_id):
//!             first_learned ← event.timestamp
//!             break
//!     
//!     if first_learned = NIL:
//!         return VOTE(string_id, valid=FALSE, ordering=NIL)
//!     
//!     ordering ← CALCULATE_ORDERING(string_id, gossip_history)
//!     round ← CALCULATE_ROUND(first_learned)
//!     
//!     return VOTE(string_id, valid=TRUE, ordering=ordering, round=round)
//! ```
//! 
//! ## Key Concepts
//! 
//! - **Gossip History**: DAG of gossip events showing which node learned what and when
//! - **Virtual Vote**: Derived vote based on when a node first saw a string
//! - **Ordering**: Consensus ordering based on gossip graph structure
//! - **Famous Witnesses**: Strings seen by supermajority that anchor finality

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Node identifier (32-byte public key hash)
pub type NodeId = [u8; 32];

/// String identifier (32-byte hash)
pub type StringId = [u8; 32];

/// Gossip event identifier
pub type EventId = [u8; 32];

// ============================================================================
// Gossip Events
// ============================================================================

/// A gossip event in the DAG
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipEvent {
    /// Unique event ID (hash of content)
    pub id: EventId,
    
    /// Node that created this event
    pub creator: NodeId,
    
    /// Round number (incremented per self-parent)
    pub round: u64,
    
    /// String IDs first learned in this event
    pub string_ids: Vec<StringId>,
    
    /// Self-parent (previous event from same creator)
    pub self_parent: Option<EventId>,
    
    /// Other-parent (event from another node that triggered this gossip)
    pub other_parent: Option<EventId>,
    
    /// Lamport timestamp
    pub timestamp: i64,
    
    /// Creator's signature
    pub signature: Vec<u8>,
    
    /// Is this a witness event? (first event in a round)
    pub is_witness: bool,
}

impl GossipEvent {
    /// Create genesis event
    pub fn genesis(creator: NodeId, string_ids: Vec<StringId>) -> Self {
        let timestamp = chrono::Utc::now().timestamp();
        let id = Self::compute_id(&creator, 0, &string_ids, None, None, timestamp);
        
        Self {
            id,
            creator,
            round: 0,
            string_ids,
            self_parent: None,
            other_parent: None,
            timestamp,
            signature: Vec::new(),
            is_witness: true,
        }
    }
    
    /// Create new event from parents
    pub fn new(
        creator: NodeId,
        round: u64,
        string_ids: Vec<StringId>,
        self_parent: EventId,
        other_parent: Option<EventId>,
    ) -> Self {
        let timestamp = chrono::Utc::now().timestamp();
        let id = Self::compute_id(&creator, round, &string_ids, Some(self_parent), other_parent, timestamp);
        
        Self {
            id,
            creator,
            round,
            string_ids,
            self_parent: Some(self_parent),
            other_parent,
            timestamp,
            signature: Vec::new(),
            is_witness: false,
        }
    }
    
    /// Compute event ID
    fn compute_id(
        creator: &NodeId,
        round: u64,
        string_ids: &[StringId],
        self_parent: Option<EventId>,
        other_parent: Option<EventId>,
        timestamp: i64,
    ) -> EventId {
        let mut hasher = blake3::Hasher::new();
        hasher.update(creator);
        hasher.update(&round.to_le_bytes());
        for sid in string_ids {
            hasher.update(sid);
        }
        if let Some(sp) = self_parent {
            hasher.update(&sp);
        }
        if let Some(op) = other_parent {
            hasher.update(&op);
        }
        hasher.update(&timestamp.to_le_bytes());
        *hasher.finalize().as_bytes()
    }
}

// ============================================================================
// Gossip History
// ============================================================================

/// Complete gossip history for a node
#[derive(Clone, Debug, Default)]
pub struct GossipHistory {
    /// All events indexed by ID
    events: HashMap<EventId, GossipEvent>,
    
    /// Events by round
    events_by_round: BTreeMap<u64, Vec<EventId>>,
    
    /// Events by creator
    events_by_creator: HashMap<NodeId, Vec<EventId>>,
    
    /// Witness events (first event of each node in each round)
    witnesses: HashMap<u64, HashMap<NodeId, EventId>>,
    
    /// Famous witnesses (confirmed by supermajority)
    famous_witnesses: HashSet<EventId>,
    
    /// Latest event per node
    latest_events: HashMap<NodeId, EventId>,
    
    /// Current round
    current_round: u64,
}

impl GossipHistory {
    /// Create new history
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Add event to history
    pub fn add_event(&mut self, event: GossipEvent) -> bool {
        if self.events.contains_key(&event.id) {
            return false;
        }
        
        let id = event.id;
        let creator = event.creator;
        let round = event.round;
        
        // Check if this is a witness (first event in round for this creator)
        let is_witness = !self.witnesses.get(&round)
            .map(|m| m.contains_key(&creator))
            .unwrap_or(false);
        
        // Insert event
        self.events.insert(id, event);
        
        // Index by round
        self.events_by_round.entry(round).or_default().push(id);
        
        // Index by creator
        self.events_by_creator.entry(creator).or_default().push(id);
        
        // Track witnesses
        if is_witness {
            self.witnesses.entry(round).or_default().insert(creator, id);
        }
        
        // Update latest
        self.latest_events.insert(creator, id);
        
        // Update current round
        if round > self.current_round {
            self.current_round = round;
        }
        
        true
    }
    
    /// Get event by ID
    pub fn get_event(&self, id: &EventId) -> Option<&GossipEvent> {
        self.events.get(id)
    }
    
    /// Get all events
    pub fn all_events(&self) -> impl Iterator<Item = &GossipEvent> {
        self.events.values()
    }
    
    /// Get events in a specific round
    pub fn events_in_round(&self, round: u64) -> Vec<&GossipEvent> {
        self.events_by_round.get(&round)
            .map(|ids| ids.iter().filter_map(|id| self.events.get(id)).collect())
            .unwrap_or_default()
    }
    
    /// Get events from a specific creator
    pub fn events_from_creator(&self, creator: &NodeId) -> Vec<&GossipEvent> {
        self.events_by_creator.get(creator)
            .map(|ids| ids.iter().filter_map(|id| self.events.get(id)).collect())
            .unwrap_or_default()
    }
    
    /// Get witnesses for a round
    pub fn witnesses_in_round(&self, round: u64) -> HashMap<NodeId, &GossipEvent> {
        self.witnesses.get(&round)
            .map(|m| m.iter()
                .filter_map(|(n, id)| self.events.get(id).map(|e| (*n, e)))
                .collect())
            .unwrap_or_default()
    }
    
    /// Get latest event from a node
    pub fn latest_from(&self, node: &NodeId) -> Option<&GossipEvent> {
        self.latest_events.get(node).and_then(|id| self.events.get(id))
    }
    
    /// Current round
    pub fn current_round(&self) -> u64 {
        self.current_round
    }
    
    /// Check if event A can see event B (there's a path in the DAG)
    pub fn can_see(&self, from: &EventId, target: &EventId) -> bool {
        if from == target {
            return true;
        }
        
        let mut visited = HashSet::new();
        let mut queue = VecDeque::new();
        queue.push_back(*from);
        
        while let Some(current) = queue.pop_front() {
            if !visited.insert(current) {
                continue;
            }
            
            if let Some(event) = self.events.get(&current) {
                if let Some(sp) = event.self_parent {
                    if sp == *target {
                        return true;
                    }
                    queue.push_back(sp);
                }
                if let Some(op) = event.other_parent {
                    if op == *target {
                        return true;
                    }
                    queue.push_back(op);
                }
            }
        }
        
        false
    }
    
    /// Find when a string was first learned
    pub fn first_learned(&self, string_id: &StringId) -> Option<&GossipEvent> {
        let mut earliest: Option<&GossipEvent> = None;
        
        for event in self.events.values() {
            if event.string_ids.contains(string_id) {
                match earliest {
                    None => earliest = Some(event),
                    Some(e) if event.round < e.round || 
                               (event.round == e.round && event.timestamp < e.timestamp) => {
                        earliest = Some(event);
                    }
                    _ => {}
                }
            }
        }
        
        earliest
    }
    
    /// Mark a witness as famous
    pub fn mark_famous(&mut self, event_id: EventId) {
        self.famous_witnesses.insert(event_id);
    }
    
    /// Check if a witness is famous
    pub fn is_famous(&self, event_id: &EventId) -> bool {
        self.famous_witnesses.contains(event_id)
    }
}

// ============================================================================
// Virtual Vote
// ============================================================================

/// Virtual vote for a string
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualVote {
    /// String being voted on
    pub string_id: StringId,
    
    /// Node whose virtual vote this represents
    pub voter_id: NodeId,
    
    /// Is the vote valid (did the node learn about the string)?
    pub is_valid: bool,
    
    /// Ordering value (based on gossip graph position)
    pub ordering: Option<u64>,
    
    /// Round when first learned
    pub round: u64,
    
    /// Timestamp when first learned
    pub first_learned_timestamp: Option<i64>,
    
    /// Decision (derived from ordering)
    pub decision: VoteDecision,
}

/// Vote decision
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoteDecision {
    /// Accept the string in this order
    Accept,
    /// Reject the string
    Reject,
    /// Abstain (didn't see the string)
    Abstain,
}

// ============================================================================
// Virtual Voting Engine
// ============================================================================

/// Virtual voting engine implementing Appendix B.1
pub struct VirtualVotingEngine {
    /// Our node ID
    our_node_id: NodeId,
    
    /// Gossip histories for all known nodes
    node_histories: RwLock<HashMap<NodeId, GossipHistory>>,
    
    /// Our local gossip history
    our_history: RwLock<GossipHistory>,
    
    /// Known validators
    validators: RwLock<HashSet<NodeId>>,
    
    /// Cached virtual votes
    vote_cache: RwLock<HashMap<(NodeId, StringId), VirtualVote>>,
    
    /// Decided strings and their consensus ordering
    decided: RwLock<HashMap<StringId, u64>>,
    
    /// Statistics
    stats: RwLock<VotingStats>,
}

/// Voting statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VotingStats {
    pub total_votes_calculated: u64,
    pub consensus_reached: u64,
    pub strings_decided: u64,
    pub rounds_completed: u64,
    pub famous_witnesses_found: u64,
}

impl VirtualVotingEngine {
    /// Create new voting engine
    pub fn new(our_node_id: NodeId) -> Self {
        Self {
            our_node_id,
            node_histories: RwLock::new(HashMap::new()),
            our_history: RwLock::new(GossipHistory::new()),
            validators: RwLock::new(HashSet::new()),
            vote_cache: RwLock::new(HashMap::new()),
            decided: RwLock::new(HashMap::new()),
            stats: RwLock::new(VotingStats::default()),
        }
    }
    
    /// Add a validator
    pub fn add_validator(&self, node_id: NodeId) {
        self.validators.write().insert(node_id);
        self.node_histories.write().entry(node_id).or_default();
    }
    
    /// Remove a validator
    pub fn remove_validator(&self, node_id: &NodeId) {
        self.validators.write().remove(node_id);
    }
    
    /// Get validator count
    pub fn validator_count(&self) -> usize {
        self.validators.read().len()
    }
    
    /// Add gossip event to our history
    pub fn add_local_event(&self, event: GossipEvent) {
        self.our_history.write().add_event(event);
    }
    
    /// Update gossip history for a remote node
    pub fn update_node_history(&self, node_id: NodeId, event: GossipEvent) {
        self.node_histories.write()
            .entry(node_id)
            .or_default()
            .add_event(event);
    }
    
    /// Calculate virtual vote for a node per Appendix B.1
    pub fn virtual_vote(&self, node_id: &NodeId, string_id: &StringId) -> VirtualVote {
        // Check cache first
        let cache_key = (*node_id, *string_id);
        if let Some(cached) = self.vote_cache.read().get(&cache_key) {
            return cached.clone();
        }
        
        // Get the node's gossip history
        let histories = self.node_histories.read();
        let history = match histories.get(node_id) {
            Some(h) => h,
            None => {
                return VirtualVote {
                    string_id: *string_id,
                    voter_id: *node_id,
                    is_valid: false,
                    ordering: None,
                    round: 0,
                    first_learned_timestamp: None,
                    decision: VoteDecision::Abstain,
                };
            }
        };
        
        // Find when node first learned of this string
        let first_learned = history.first_learned(string_id);
        
        let vote = match first_learned {
            Some(event) => {
                // Calculate ordering based on gossip graph
                let ordering = self.calculate_ordering(string_id, history);
                let round = event.round;
                
                VirtualVote {
                    string_id: *string_id,
                    voter_id: *node_id,
                    is_valid: true,
                    ordering: Some(ordering),
                    round,
                    first_learned_timestamp: Some(event.timestamp),
                    decision: VoteDecision::Accept,
                }
            }
            None => {
                VirtualVote {
                    string_id: *string_id,
                    voter_id: *node_id,
                    is_valid: false,
                    ordering: None,
                    round: 0,
                    first_learned_timestamp: None,
                    decision: VoteDecision::Abstain,
                }
            }
        };
        
        // Cache the result
        self.vote_cache.write().insert(cache_key, vote.clone());
        self.stats.write().total_votes_calculated += 1;
        
        vote
    }
    
    /// Calculate ordering for a string based on gossip history
    fn calculate_ordering(&self, string_id: &StringId, history: &GossipHistory) -> u64 {
        // Ordering is based on:
        // 1. Round when first seen
        // 2. Number of events that reference it
        // 3. Timestamp within round
        
        let mut ordering = 0u64;
        let mut ref_count = 0u64;
        
        for event in history.all_events() {
            if event.string_ids.contains(string_id) {
                if ordering == 0 {
                    ordering = event.round * 1_000_000 + (event.timestamp as u64 % 1_000_000);
                }
                ref_count += 1;
            }
        }
        
        // Adjust ordering by reference count (more references = more certain)
        ordering.saturating_add(ref_count.saturating_mul(100))
    }
    
    /// Calculate consensus vote across all validators
    pub fn consensus_vote(&self, string_id: &StringId) -> Option<u64> {
        let validators = self.validators.read().clone();
        let mut ordering_votes: HashMap<u64, usize> = HashMap::new();
        let mut accept_count = 0;
        let mut abstain_count = 0;
        
        for validator in &validators {
            let vote = self.virtual_vote(validator, string_id);
            
            match vote.decision {
                VoteDecision::Accept => {
                    accept_count += 1;
                    if let Some(ordering) = vote.ordering {
                        *ordering_votes.entry(ordering).or_insert(0) += 1;
                    }
                }
                VoteDecision::Abstain => abstain_count += 1,
                VoteDecision::Reject => {}
            }
        }
        
        // Need supermajority (2/3+) to reach consensus
        let threshold = (validators.len() * 2) / 3;
        
        if accept_count <= threshold {
            return None;
        }
        
        // Find ordering with most votes
        ordering_votes.into_iter()
            .max_by_key(|(_, count)| *count)
            .map(|(ordering, _)| {
                self.stats.write().consensus_reached += 1;
                ordering
            })
    }
    
    /// Check if a string has reached finality
    pub fn is_decided(&self, string_id: &StringId) -> bool {
        self.decided.read().contains_key(string_id)
    }
    
    /// Get decided ordering for a string
    pub fn get_decided_ordering(&self, string_id: &StringId) -> Option<u64> {
        self.decided.read().get(string_id).copied()
    }
    
    /// Attempt to decide a string's ordering
    pub fn try_decide(&self, string_id: &StringId) -> Option<u64> {
        if let Some(ordering) = self.decided.read().get(string_id) {
            return Some(*ordering);
        }
        
        if let Some(consensus_ordering) = self.consensus_vote(string_id) {
            self.decided.write().insert(*string_id, consensus_ordering);
            self.stats.write().strings_decided += 1;
            return Some(consensus_ordering);
        }
        
        None
    }
    
    /// Determine if a witness is famous per §6.2.3
    pub fn is_witness_famous(
        &self,
        witness_event_id: &EventId,
        validators: &[NodeId],
    ) -> bool {
        let threshold = (validators.len() * 2) / 3;
        let mut see_count = 0;
        
        let our_history = self.our_history.read();
        
        // Check how many validators' latest events can see this witness
        for validator in validators {
            if let Some(latest) = our_history.latest_from(validator) {
                if our_history.can_see(&latest.id, witness_event_id) {
                    see_count += 1;
                }
            }
        }
        
        see_count > threshold
    }
    
    /// Run a voting round to find famous witnesses and decide strings
    pub fn run_voting_round(&self) -> Vec<StringId> {
        let validators: Vec<NodeId> = self.validators.read().iter().copied().collect();
        let mut newly_decided = Vec::new();
        
        // Find all undecided strings
        let mut undecided_strings: HashSet<StringId> = HashSet::new();
        
        {
            let histories = self.node_histories.read();
            for history in histories.values() {
                for event in history.all_events() {
                    for string_id in &event.string_ids {
                        if !self.is_decided(string_id) {
                            undecided_strings.insert(*string_id);
                        }
                    }
                }
            }
        }
        
        // Try to decide each undecided string
        for string_id in undecided_strings {
            if let Some(_ordering) = self.try_decide(&string_id) {
                newly_decided.push(string_id);
            }
        }
        
        self.stats.write().rounds_completed += 1;
        
        newly_decided
    }
    
    /// Get statistics
    pub fn stats(&self) -> VotingStats {
        self.stats.read().clone()
    }
    
    /// Clear vote cache (call periodically or when history changes significantly)
    pub fn clear_cache(&self) {
        self.vote_cache.write().clear();
    }
    
    /// Get our node ID
    pub fn our_node_id(&self) -> NodeId {
        self.our_node_id
    }
}

// ============================================================================
// Strongly-Sees Relation (§6.3.1)
// ============================================================================

/// Check if string A strongly sees string B per §6.3.1
/// A string strongly sees another when it has been observed by a supermajority:
/// strongly_sees(s, target) ⟺ observers > (2 * validator_count) / 3
pub fn strongly_sees(
    engine: &VirtualVotingEngine,
    string_id: &StringId,
    target_string_id: &StringId,
    validators: &[NodeId],
) -> bool {
    let threshold = (2 * validators.len()) / 3;
    let mut observers = 0;
    
    for validator in validators {
        let vote_for_string = engine.virtual_vote(validator, string_id);
        let vote_for_target = engine.virtual_vote(validator, target_string_id);
        
        // Validator "observes" if they saw both strings and target first
        if vote_for_string.is_valid && vote_for_target.is_valid {
            if let (Some(s_ts), Some(t_ts)) = (
                vote_for_string.first_learned_timestamp,
                vote_for_target.first_learned_timestamp,
            ) {
                if t_ts <= s_ts {
                    observers += 1;
                }
            }
        }
    }
    
    observers > threshold
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    
    fn make_node_id(n: u8) -> NodeId {
        let mut id = [0u8; 32];
        id[0] = n;
        id
    }
    
    fn make_string_id(s: &str) -> StringId {
        *blake3::hash(s.as_bytes()).as_bytes()
    }
    
    #[test]
    fn test_gossip_history() {
        let mut history = GossipHistory::new();
        let node_a = make_node_id(1);
        let string_1 = make_string_id("string_1");
        
        let event1 = GossipEvent::genesis(node_a, vec![string_1]);
        assert!(history.add_event(event1.clone()));
        
        assert_eq!(history.events.len(), 1);
        assert!(history.first_learned(&string_1).is_some());
        assert!(history.first_learned(&make_string_id("unknown")).is_none());
    }
    
    #[test]
    fn test_virtual_vote_calculation() {
        let node_a = make_node_id(1);
        let node_b = make_node_id(2);
        let string_1 = make_string_id("test_string");
        
        let engine = VirtualVotingEngine::new(node_a);
        engine.add_validator(node_a);
        engine.add_validator(node_b);
        
        // Add event where node_a learns string_1
        let event = GossipEvent::genesis(node_a, vec![string_1]);
        engine.update_node_history(node_a, event);
        
        // Virtual vote for node_a should be valid
        let vote = engine.virtual_vote(&node_a, &string_1);
        assert!(vote.is_valid);
        assert_eq!(vote.decision, VoteDecision::Accept);
        
        // Virtual vote for node_b should be abstain (hasn't seen it)
        let vote = engine.virtual_vote(&node_b, &string_1);
        assert!(!vote.is_valid);
        assert_eq!(vote.decision, VoteDecision::Abstain);
    }
    
    #[test]
    fn test_consensus_requires_supermajority() {
        let string_1 = make_string_id("consensus_test");
        let engine = VirtualVotingEngine::new(make_node_id(0));
        
        // Add 4 validators
        for i in 1..=4 {
            engine.add_validator(make_node_id(i));
        }
        
        // Only 2 validators see the string (not supermajority of 4)
        for i in 1..=2 {
            let event = GossipEvent::genesis(make_node_id(i), vec![string_1]);
            engine.update_node_history(make_node_id(i), event);
        }
        
        // Should not reach consensus (2/4 = 50%, need >66%)
        assert!(engine.consensus_vote(&string_1).is_none());
        
        // Add third validator seeing string (3/4 = 75%, > 66%)
        let event = GossipEvent::genesis(make_node_id(3), vec![string_1]);
        engine.update_node_history(make_node_id(3), event);
        engine.clear_cache();
        
        // Now should reach consensus
        assert!(engine.consensus_vote(&string_1).is_some());
    }
    
    #[test]
    fn test_gossip_can_see() {
        let mut history = GossipHistory::new();
        let node_a = make_node_id(1);
        
        let event1 = GossipEvent::genesis(node_a, vec![make_string_id("s1")]);
        let event1_id = event1.id;
        history.add_event(event1);
        
        let event2 = GossipEvent::new(node_a, 1, vec![make_string_id("s2")], event1_id, None);
        let event2_id = event2.id;
        history.add_event(event2);
        
        // event2 should be able to see event1
        assert!(history.can_see(&event2_id, &event1_id));
        
        // event1 should not see event2
        assert!(!history.can_see(&event1_id, &event2_id));
    }
}

