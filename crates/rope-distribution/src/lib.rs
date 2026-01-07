//! # Rope Distribution Protocol (RDP)
//! 
//! BitTorrent-inspired distribution mechanism for strings in the lattice.
//! 
//! ## Components
//! 
//! - **Swarm**: Nodes interested in a string family
//! - **Tracker**: Distributed tracker using system strings  
//! - **DHT**: Semantic distributed hash table
//! - **Incentives**: Token-based rewards for contribution

pub mod rdp {
    //! Core RDP protocol
    //! 
    //! Optimized for distributing strings and their complements
    //! across the network with configurable redundancy.
    
    use std::collections::HashMap;
    
    /// RDP chunk for distribution
    #[derive(Clone, Debug)]
    pub struct RdpChunk {
        pub string_id: [u8; 32],
        pub chunk_index: u32,
        pub total_chunks: u32,
        pub data: Vec<u8>,
        pub checksum: [u8; 32],
    }
    
    /// RDP transfer state
    pub struct RdpTransfer {
        pub string_id: [u8; 32],
        pub received_chunks: HashMap<u32, RdpChunk>,
        pub total_chunks: u32,
    }
    
    impl RdpTransfer {
        pub fn new(string_id: [u8; 32], total_chunks: u32) -> Self {
            Self {
                string_id,
                received_chunks: HashMap::new(),
                total_chunks,
            }
        }
        
        pub fn add_chunk(&mut self, chunk: RdpChunk) {
            self.received_chunks.insert(chunk.chunk_index, chunk);
        }
        
        pub fn is_complete(&self) -> bool {
            self.received_chunks.len() as u32 == self.total_chunks
        }
        
        pub fn progress(&self) -> f32 {
            self.received_chunks.len() as f32 / self.total_chunks as f32
        }
    }
}

pub mod swarm {
    //! Swarm management
    //! 
    //! A swarm consists of nodes interested in a particular string family.
    //! Nodes can be seeders (have complete data) or leechers (downloading).
    
    use std::collections::{HashMap, HashSet};
    use std::time::Instant;
    
    /// Swarm member
    #[derive(Clone, Debug)]
    pub struct SwarmMember {
        pub node_id: [u8; 32],
        pub is_seeder: bool,
        pub upload_speed: u64,
        pub download_speed: u64,
        pub last_seen: u64,
    }
    
    /// Swarm for a string family
    pub struct Swarm {
        pub family_id: [u8; 32],
        pub members: HashMap<[u8; 32], SwarmMember>,
        pub seeders: HashSet<[u8; 32]>,
        pub leechers: HashSet<[u8; 32]>,
    }
    
    impl Swarm {
        pub fn new(family_id: [u8; 32]) -> Self {
            Self {
                family_id,
                members: HashMap::new(),
                seeders: HashSet::new(),
                leechers: HashSet::new(),
            }
        }
        
        pub fn add_member(&mut self, member: SwarmMember) {
            let is_seeder = member.is_seeder;
            let node_id = member.node_id;
            
            self.members.insert(node_id, member);
            
            if is_seeder {
                self.seeders.insert(node_id);
                self.leechers.remove(&node_id);
            } else {
                self.leechers.insert(node_id);
            }
        }
        
        pub fn member_count(&self) -> usize {
            self.members.len()
        }
        
        pub fn seeder_count(&self) -> usize {
            self.seeders.len()
        }
    }
}

pub mod dht {
    //! Semantic DHT
    //! 
    //! Distributed hash table with semantic awareness:
    //! - Content-based routing
    //! - Domain-aware partitioning
    //! - Efficient range queries for related strings
    
    use std::collections::HashMap;
    
    /// DHT node entry
    #[derive(Clone, Debug)]
    pub struct DhtEntry {
        pub key: [u8; 32],
        pub value: Vec<u8>,
        pub ttl_seconds: u64,
        pub domain: String,
    }
    
    /// Simple local DHT storage
    pub struct DhtStore {
        entries: HashMap<[u8; 32], DhtEntry>,
    }
    
    impl DhtStore {
        pub fn new() -> Self {
            Self {
                entries: HashMap::new(),
            }
        }
        
        pub fn put(&mut self, entry: DhtEntry) {
            self.entries.insert(entry.key, entry);
        }
        
        pub fn get(&self, key: &[u8; 32]) -> Option<&DhtEntry> {
            self.entries.get(key)
        }
        
        pub fn find_by_domain(&self, domain: &str) -> Vec<&DhtEntry> {
            self.entries.values()
                .filter(|e| e.domain == domain)
                .collect()
        }
    }
    
    impl Default for DhtStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod incentives {
    //! Reward calculation: α×bandwidth + β×storage + γ×regeneration
    //! 
    //! Nodes are rewarded for:
    //! - Providing bandwidth (seeding)
    //! - Storing strings and complements
    //! - Participating in regeneration
    
    /// Incentive parameters
    #[derive(Clone, Debug)]
    pub struct IncentiveParams {
        /// Weight for bandwidth contribution
        pub alpha: f64,
        /// Weight for storage contribution
        pub beta: f64,
        /// Weight for regeneration participation
        pub gamma: f64,
        /// Base reward per epoch
        pub base_reward: u64,
    }
    
    impl Default for IncentiveParams {
        fn default() -> Self {
            Self {
                alpha: 0.4,
                beta: 0.4,
                gamma: 0.2,
                base_reward: 100,
            }
        }
    }
    
    /// Node contribution metrics
    #[derive(Clone, Debug, Default)]
    pub struct NodeContribution {
        pub bytes_uploaded: u64,
        pub bytes_stored: u64,
        pub regenerations_helped: u64,
        pub uptime_seconds: u64,
    }
    
    /// Calculate reward for a node
    pub fn calculate_reward(params: &IncentiveParams, contrib: &NodeContribution) -> u64 {
        let bandwidth_score = (contrib.bytes_uploaded as f64).sqrt();
        let storage_score = (contrib.bytes_stored as f64).sqrt();
        let regen_score = contrib.regenerations_helped as f64 * 10.0;
        
        let total_score = params.alpha * bandwidth_score 
            + params.beta * storage_score 
            + params.gamma * regen_score;
            
        (params.base_reward as f64 * total_score.sqrt()) as u64
    }
}

// Re-exports
pub use rdp::{RdpChunk, RdpTransfer};
pub use swarm::{Swarm, SwarmMember};
pub use dht::{DhtEntry, DhtStore};
pub use incentives::{IncentiveParams, NodeContribution, calculate_reward};
