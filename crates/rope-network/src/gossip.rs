//! # Gossip-about-Gossip Protocol
//! 
//! Implements the gossip protocol for propagating strings and testimonies.
//! Based on Hashgraph's gossip-about-gossip with batching optimizations.
//! 
//! ## Protocol Overview
//! 
//! 1. Node A selects random peer B
//! 2. A sends: "I have strings S1, S2, ... Sn"
//! 3. B responds: "Send me S2, S5" (ones B doesn't have)
//! 4. A sends full string data for S2, S5
//! 5. Both record the gossip event for virtual voting
//! 
//! ## Batching
//! 
//! - Maximum 1000 strings per gossip message
//! - Gossip every 100ms or when batch is full

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use parking_lot::RwLock;
use rope_core::types::StringId;

/// Gossip protocol configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipConfig {
    /// Gossip interval
    pub gossip_interval: Duration,
    
    /// Maximum strings per gossip message
    pub max_batch_size: usize,
    
    /// Fanout (number of peers to gossip to per round)
    pub fanout: usize,
    
    /// Maximum gossip history to keep
    pub max_history: usize,
    
    /// Enable gossip compression
    pub enable_compression: bool,
}

impl Default for GossipConfig {
    fn default() -> Self {
        Self {
            gossip_interval: Duration::from_millis(100),
            max_batch_size: 1000,
            fanout: 10,
            max_history: 10000,
            enable_compression: true,
        }
    }
}

/// Gossip message types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum GossipMessageType {
    /// Announce available strings
    Have(Vec<StringId>),
    
    /// Request specific strings
    Want(Vec<StringId>),
    
    /// Send string data
    Data(Vec<StringData>),
    
    /// Sync request (full state)
    SyncRequest { from_round: u64 },
    
    /// Sync response
    SyncResponse { strings: Vec<StringData>, round: u64 },
}

/// String data for gossip
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StringData {
    pub id: StringId,
    pub content: Vec<u8>,
    pub signature: Vec<u8>,
    pub timestamp: i64,
}

/// Gossip message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipMessage {
    /// Sender node ID
    pub sender_id: [u8; 32],
    
    /// Message sequence number
    pub sequence: u64,
    
    /// Parent gossip hashes (gossip-about-gossip)
    pub parent_hashes: Vec<[u8; 32]>,
    
    /// Message type and payload
    pub message_type: GossipMessageType,
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Message hash
    pub hash: [u8; 32],
}

impl GossipMessage {
    /// Create a new gossip message
    pub fn new(
        sender_id: [u8; 32],
        sequence: u64,
        parent_hashes: Vec<[u8; 32]>,
        message_type: GossipMessageType,
    ) -> Self {
        let timestamp = chrono::Utc::now().timestamp();
        
        // Calculate hash
        let mut data = sender_id.to_vec();
        data.extend_from_slice(&sequence.to_le_bytes());
        data.extend_from_slice(&timestamp.to_le_bytes());
        let hash = *blake3::hash(&data).as_bytes();
        
        Self {
            sender_id,
            sequence,
            parent_hashes,
            message_type,
            timestamp,
            hash,
        }
    }
}

/// Gossip event for virtual voting
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipEvent {
    pub hash: [u8; 32],
    pub sender_id: [u8; 32],
    pub receiver_id: [u8; 32],
    pub round: u64,
    pub timestamp: i64,
    pub parent_events: Vec<[u8; 32]>,
    pub string_ids: Vec<StringId>,
}

/// Gossip history for virtual voting reconstruction
pub struct GossipHistory {
    events: VecDeque<GossipEvent>,
    event_index: HashMap<[u8; 32], usize>,
    max_history: usize,
}

impl GossipHistory {
    /// Create new gossip history
    pub fn new(max_history: usize) -> Self {
        Self {
            events: VecDeque::new(),
            event_index: HashMap::new(),
            max_history,
        }
    }
    
    /// Add a gossip event
    pub fn add_event(&mut self, event: GossipEvent) {
        let index = self.events.len();
        self.event_index.insert(event.hash, index);
        self.events.push_back(event);
        
        // Trim old events
        while self.events.len() > self.max_history {
            if let Some(old) = self.events.pop_front() {
                self.event_index.remove(&old.hash);
            }
        }
    }
    
    /// Get event by hash
    pub fn get_event(&self, hash: &[u8; 32]) -> Option<&GossipEvent> {
        self.event_index.get(hash)
            .and_then(|&idx| self.events.get(idx))
    }
    
    /// Get all events in a round
    pub fn events_in_round(&self, round: u64) -> Vec<&GossipEvent> {
        self.events.iter()
            .filter(|e| e.round == round)
            .collect()
    }
    
    /// Get events by sender
    pub fn events_by_sender(&self, sender_id: &[u8; 32]) -> Vec<&GossipEvent> {
        self.events.iter()
            .filter(|e| &e.sender_id == sender_id)
            .collect()
    }
    
    /// Check if event exists
    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.event_index.contains_key(hash)
    }
    
    /// Get all events
    pub fn all_events(&self) -> &VecDeque<GossipEvent> {
        &self.events
    }
    
    /// Get latest round
    pub fn latest_round(&self) -> u64 {
        self.events.back()
            .map(|e| e.round)
            .unwrap_or(0)
    }
}

/// Gossip protocol manager
pub struct GossipProtocol {
    /// Configuration
    config: GossipConfig,
    
    /// Node ID
    node_id: [u8; 32],
    
    /// Current sequence number
    sequence: RwLock<u64>,
    
    /// Known strings (for deduplication)
    known_strings: RwLock<HashSet<StringId>>,
    
    /// Pending strings to gossip
    pending_strings: RwLock<Vec<StringData>>,
    
    /// Gossip history
    history: RwLock<GossipHistory>,
    
    /// Last gossip time
    last_gossip: RwLock<Instant>,
    
    /// Statistics
    stats: RwLock<GossipStats>,
}

/// Gossip statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GossipStats {
    pub messages_sent: u64,
    pub messages_received: u64,
    pub strings_propagated: u64,
    pub duplicates_received: u64,
    pub current_round: u64,
}

impl GossipProtocol {
    /// Create new gossip protocol
    pub fn new(node_id: [u8; 32], config: GossipConfig) -> Self {
        let max_history = config.max_history;
        Self {
            config,
            node_id,
            sequence: RwLock::new(0),
            known_strings: RwLock::new(HashSet::new()),
            pending_strings: RwLock::new(Vec::new()),
            history: RwLock::new(GossipHistory::new(max_history)),
            last_gossip: RwLock::new(Instant::now()),
            stats: RwLock::new(GossipStats::default()),
        }
    }
    
    /// Add a string to propagate
    pub fn add_string(&self, data: StringData) {
        let mut known = self.known_strings.write();
        if known.insert(data.id) {
            self.pending_strings.write().push(data);
        }
    }
    
    /// Check if we should gossip now
    pub fn should_gossip(&self) -> bool {
        let elapsed = self.last_gossip.read().elapsed();
        let pending_count = self.pending_strings.read().len();
        
        elapsed >= self.config.gossip_interval || pending_count >= self.config.max_batch_size
    }
    
    /// Create a Have message for gossip
    pub fn create_have_message(&self) -> Option<GossipMessage> {
        let pending = self.pending_strings.read();
        if pending.is_empty() {
            return None;
        }
        
        let string_ids: Vec<StringId> = pending.iter()
            .take(self.config.max_batch_size)
            .map(|s| s.id)
            .collect();
        
        let mut seq = self.sequence.write();
        *seq += 1;
        
        let parent_hashes = self.get_recent_event_hashes();
        
        Some(GossipMessage::new(
            self.node_id,
            *seq,
            parent_hashes,
            GossipMessageType::Have(string_ids),
        ))
    }
    
    /// Handle incoming gossip message
    pub fn handle_message(&self, msg: GossipMessage) -> Option<GossipMessage> {
        self.stats.write().messages_received += 1;
        
        match msg.message_type {
            GossipMessageType::Have(ids) => {
                // Check which strings we need
                let known = self.known_strings.read();
                let want: Vec<StringId> = ids.into_iter()
                    .filter(|id| !known.contains(id))
                    .collect();
                
                if want.is_empty() {
                    return None;
                }
                
                let mut seq = self.sequence.write();
                *seq += 1;
                
                Some(GossipMessage::new(
                    self.node_id,
                    *seq,
                    vec![msg.hash],
                    GossipMessageType::Want(want),
                ))
            }
            
            GossipMessageType::Want(ids) => {
                // Send requested strings
                let pending = self.pending_strings.read();
                let data: Vec<StringData> = pending.iter()
                    .filter(|s| ids.contains(&s.id))
                    .cloned()
                    .collect();
                
                if data.is_empty() {
                    return None;
                }
                
                let mut seq = self.sequence.write();
                *seq += 1;
                
                Some(GossipMessage::new(
                    self.node_id,
                    *seq,
                    vec![msg.hash],
                    GossipMessageType::Data(data),
                ))
            }
            
            GossipMessageType::Data(strings) => {
                // Receive strings
                let mut known = self.known_strings.write();
                let mut stats = self.stats.write();
                
                for string in strings {
                    if known.insert(string.id) {
                        stats.strings_propagated += 1;
                        // In production: add to lattice
                    } else {
                        stats.duplicates_received += 1;
                    }
                }
                
                None
            }
            
            GossipMessageType::SyncRequest { from_round } => {
                // Handle sync request
                let history = self.history.read();
                let mut strings = Vec::new();
                
                for event in history.all_events() {
                    if event.round >= from_round {
                        // In production: fetch actual string data
                        for id in &event.string_ids {
                            strings.push(StringData {
                                id: *id,
                                content: Vec::new(),
                                signature: Vec::new(),
                                timestamp: event.timestamp,
                            });
                        }
                    }
                }
                
                let mut seq = self.sequence.write();
                *seq += 1;
                
                Some(GossipMessage::new(
                    self.node_id,
                    *seq,
                    vec![msg.hash],
                    GossipMessageType::SyncResponse {
                        strings,
                        round: history.latest_round(),
                    },
                ))
            }
            
            GossipMessageType::SyncResponse { strings, round: _ } => {
                // Receive sync data
                let mut known = self.known_strings.write();
                for string in strings {
                    known.insert(string.id);
                }
                None
            }
        }
    }
    
    /// Record a gossip event for virtual voting
    pub fn record_event(
        &self,
        receiver_id: [u8; 32],
        round: u64,
        string_ids: Vec<StringId>,
    ) {
        let timestamp = chrono::Utc::now().timestamp();
        let parent_events = self.get_recent_event_hashes();
        
        let mut data = self.node_id.to_vec();
        data.extend_from_slice(&receiver_id);
        data.extend_from_slice(&round.to_le_bytes());
        data.extend_from_slice(&timestamp.to_le_bytes());
        let hash = *blake3::hash(&data).as_bytes();
        
        let event = GossipEvent {
            hash,
            sender_id: self.node_id,
            receiver_id,
            round,
            timestamp,
            parent_events,
            string_ids,
        };
        
        self.history.write().add_event(event);
        self.stats.write().current_round = round;
    }
    
    /// Get recent event hashes for parent references
    fn get_recent_event_hashes(&self) -> Vec<[u8; 32]> {
        let history = self.history.read();
        history.all_events()
            .iter()
            .rev()
            .take(2)
            .map(|e| e.hash)
            .collect()
    }
    
    /// Mark gossip as sent
    pub fn mark_gossiped(&self) {
        *self.last_gossip.write() = Instant::now();
        self.pending_strings.write().clear();
        self.stats.write().messages_sent += 1;
    }
    
    /// Get statistics
    pub fn stats(&self) -> GossipStats {
        self.stats.read().clone()
    }
    
    /// Get configuration
    pub fn config(&self) -> &GossipConfig {
        &self.config
    }
}

impl Default for GossipProtocol {
    fn default() -> Self {
        Self::new([0u8; 32], GossipConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_gossip_config() {
        let config = GossipConfig::default();
        assert_eq!(config.fanout, 10);
        assert_eq!(config.max_batch_size, 1000);
    }
    
    #[test]
    fn test_gossip_message() {
        let msg = GossipMessage::new(
            [1u8; 32],
            1,
            vec![],
            GossipMessageType::Have(vec![StringId::from_content(b"test")]),
        );
        
        assert_eq!(msg.sender_id, [1u8; 32]);
        assert_eq!(msg.sequence, 1);
    }
    
    #[test]
    fn test_gossip_history() {
        let mut history = GossipHistory::new(100);
        
        let event = GossipEvent {
            hash: [1u8; 32],
            sender_id: [1u8; 32],
            receiver_id: [2u8; 32],
            round: 1,
            timestamp: 0,
            parent_events: vec![],
            string_ids: vec![],
        };
        
        history.add_event(event.clone());
        
        assert!(history.contains(&[1u8; 32]));
        assert_eq!(history.latest_round(), 1);
    }
    
    #[test]
    fn test_gossip_protocol() {
        let protocol = GossipProtocol::new([1u8; 32], GossipConfig::default());
        
        // Add a string
        protocol.add_string(StringData {
            id: StringId::from_content(b"test"),
            content: b"test content".to_vec(),
            signature: vec![],
            timestamp: 0,
        });
        
        // Should create have message
        let msg = protocol.create_have_message();
        assert!(msg.is_some());
        
        let msg = msg.unwrap();
        assert!(matches!(msg.message_type, GossipMessageType::Have(_)));
    }
    
    #[test]
    fn test_handle_want_message() {
        let protocol = GossipProtocol::new([1u8; 32], GossipConfig::default());
        let string_id = StringId::from_content(b"test");
        
        // Add a string
        protocol.add_string(StringData {
            id: string_id,
            content: b"test content".to_vec(),
            signature: vec![],
            timestamp: 0,
        });
        
        // Create want message
        let want_msg = GossipMessage::new(
            [2u8; 32],
            1,
            vec![],
            GossipMessageType::Want(vec![string_id]),
        );
        
        // Handle want message
        let response = protocol.handle_message(want_msg);
        assert!(response.is_some());
        
        let response = response.unwrap();
        assert!(matches!(response.message_type, GossipMessageType::Data(_)));
    }
}

