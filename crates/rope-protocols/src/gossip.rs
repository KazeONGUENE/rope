//! # Gossip-about-Gossip Protocol
//! 
//! Nodes share communication history for virtual voting.
//! Each gossip event references its parents, forming a DAG.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// A gossip event
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipEvent {
    pub id: [u8; 32],
    pub creator_id: [u8; 32],
    pub self_parent: Option<[u8; 32]>,
    pub other_parent: Option<[u8; 32]>,
    pub payload: Vec<u8>,
    pub timestamp: u64,
    pub round: u64,
}

/// Gossip DAG for a node
pub struct GossipDag {
    events: HashMap<[u8; 32], GossipEvent>,
    heads: HashSet<[u8; 32]>,
    round: u64,
}

impl GossipDag {
    pub fn new() -> Self {
        Self {
            events: HashMap::new(),
            heads: HashSet::new(),
            round: 0,
        }
    }
    
    pub fn add_event(&mut self, event: GossipEvent) {
        // Remove parents from heads
        if let Some(p) = event.self_parent {
            self.heads.remove(&p);
        }
        if let Some(p) = event.other_parent {
            self.heads.remove(&p);
        }
        
        let id = event.id;
        self.heads.insert(id);
        
        if event.round > self.round {
            self.round = event.round;
        }
        
        self.events.insert(id, event);
    }
    
    pub fn get_event(&self, id: &[u8; 32]) -> Option<&GossipEvent> {
        self.events.get(id)
    }
    
    pub fn current_round(&self) -> u64 {
        self.round
    }
    
    pub fn head_events(&self) -> Vec<&GossipEvent> {
        self.heads.iter()
            .filter_map(|id| self.events.get(id))
            .collect()
    }
}

impl Default for GossipDag {
    fn default() -> Self {
        Self::new()
    }
}

