//! Lamport Clock implementation for causal ordering
//! 
//! τ (Tau) - Temporal Marker using Lamport clock extended with causal ordering.
//! 
//! Unlike synchronized wall clocks, Lamport clocks provide a logical ordering
//! that respects causality: if event A caused event B, then clock(A) < clock(B).

use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use crate::types::NodeId;

/// Extended Lamport Clock with causal parent tracking
/// 
/// This implementation extends the basic Lamport clock with:
/// - Node identification for tie-breaking
/// - Causal parent references for DAG construction
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LamportClock {
    /// Logical time counter
    logical_time: u64,
    
    /// Node that created this timestamp
    node_id: NodeId,
    
    /// Causal parents: (NodeId, logical_time) pairs
    causal_parents: Vec<(NodeId, u64)>,
}

impl LamportClock {
    /// Create a new clock for a node, starting at 0
    pub fn new(node_id: NodeId) -> Self {
        Self {
            logical_time: 0,
            node_id,
            causal_parents: Vec::new(),
        }
    }

    /// Create clock with specific time (for deserialization)
    pub fn with_time(logical_time: u64, node_id: NodeId) -> Self {
        Self {
            logical_time,
            node_id,
            causal_parents: Vec::new(),
        }
    }

    /// Get current logical time
    pub fn time(&self) -> u64 {
        self.logical_time
    }

    /// Get the node id
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get causal parents
    pub fn causal_parents(&self) -> &[(NodeId, u64)] {
        &self.causal_parents
    }

    /// Increment clock for local event
    pub fn increment(&mut self) -> u64 {
        self.logical_time += 1;
        self.causal_parents.clear();
        self.logical_time
    }

    /// Update clock upon receiving a message (observe remote clock)
    pub fn observe(&mut self, other: &LamportClock) {
        self.logical_time = self.logical_time.max(other.logical_time) + 1;
        self.causal_parents.push((other.node_id, other.logical_time));
    }

    /// Observe multiple clocks and update
    pub fn observe_many<'a>(&mut self, others: impl Iterator<Item = &'a LamportClock>) {
        let mut max_time = self.logical_time;
        
        for other in others {
            max_time = max_time.max(other.logical_time);
            self.causal_parents.push((other.node_id, other.logical_time));
        }
        
        self.logical_time = max_time + 1;
    }

    /// Create a snapshot of current state
    pub fn snapshot(&self) -> LamportClock {
        self.clone()
    }

    /// Check if this clock happened-before another
    /// 
    /// Returns true if this clock definitely precedes `other` causally
    pub fn happened_before(&self, other: &LamportClock) -> bool {
        if self.logical_time >= other.logical_time {
            return false;
        }
        
        // Check if we're in the causal parents
        other.causal_parents.iter()
            .any(|(node, time)| *node == self.node_id && *time >= self.logical_time)
    }

    /// Check if events are concurrent (neither happened before the other)
    pub fn is_concurrent(&self, other: &LamportClock) -> bool {
        !self.happened_before(other) && !other.happened_before(self)
    }

    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&self.logical_time.to_be_bytes());
        bytes.extend_from_slice(self.node_id.as_bytes());
        bytes.extend_from_slice(&(self.causal_parents.len() as u32).to_be_bytes());
        
        for (node, time) in &self.causal_parents {
            bytes.extend_from_slice(node.as_bytes());
            bytes.extend_from_slice(&time.to_be_bytes());
        }
        
        bytes
    }
}

impl PartialOrd for LamportClock {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for LamportClock {
    fn cmp(&self, other: &Self) -> Ordering {
        // Primary: logical time
        match self.logical_time.cmp(&other.logical_time) {
            Ordering::Equal => {
                // Tie-breaker: node id (for total ordering)
                self.node_id.as_bytes().cmp(other.node_id.as_bytes())
            }
            other => other,
        }
    }
}

impl Default for LamportClock {
    fn default() -> Self {
        Self {
            logical_time: 0,
            node_id: NodeId::new([0u8; 32]),
            causal_parents: Vec::new(),
        }
    }
}

/// Clock manager for a node
pub struct ClockManager {
    clock: parking_lot::Mutex<LamportClock>,
}

impl ClockManager {
    /// Create a new clock manager for a node
    pub fn new(node_id: NodeId) -> Self {
        Self {
            clock: parking_lot::Mutex::new(LamportClock::new(node_id)),
        }
    }

    /// Get current time without incrementing
    pub fn now(&self) -> LamportClock {
        self.clock.lock().snapshot()
    }

    /// Increment and get new timestamp
    pub fn tick(&self) -> LamportClock {
        let mut clock = self.clock.lock();
        clock.increment();
        clock.snapshot()
    }

    /// Update clock based on received message
    pub fn observe(&self, other: &LamportClock) -> LamportClock {
        let mut clock = self.clock.lock();
        clock.observe(other);
        clock.snapshot()
    }

    /// Update clock based on multiple messages
    pub fn observe_many<'a>(&self, others: impl Iterator<Item = &'a LamportClock>) -> LamportClock {
        let mut clock = self.clock.lock();
        clock.observe_many(others);
        clock.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_node_id(id: u8) -> NodeId {
        let mut bytes = [0u8; 32];
        bytes[0] = id;
        NodeId::new(bytes)
    }

    #[test]
    fn test_clock_increment() {
        let mut clock = LamportClock::new(make_node_id(1));
        
        assert_eq!(clock.time(), 0);
        clock.increment();
        assert_eq!(clock.time(), 1);
        clock.increment();
        assert_eq!(clock.time(), 2);
    }

    #[test]
    fn test_clock_observe() {
        let mut clock_a = LamportClock::new(make_node_id(1));
        let mut clock_b = LamportClock::new(make_node_id(2));
        
        // A increments a few times
        clock_a.increment();
        clock_a.increment();
        clock_a.increment();
        assert_eq!(clock_a.time(), 3);
        
        // B observes A's clock
        clock_b.observe(&clock_a);
        assert_eq!(clock_b.time(), 4); // max(0, 3) + 1
    }

    #[test]
    fn test_clock_ordering() {
        let mut clock_a = LamportClock::new(make_node_id(1));
        let mut clock_b = LamportClock::new(make_node_id(2));
        
        clock_a.increment();
        clock_b.observe(&clock_a);
        
        assert!(clock_a < clock_b);
    }

    #[test]
    fn test_clock_manager() {
        let manager = ClockManager::new(make_node_id(1));
        
        let t1 = manager.tick();
        let t2 = manager.tick();
        let t3 = manager.tick();
        
        assert!(t1 < t2);
        assert!(t2 < t3);
    }
}

