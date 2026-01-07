//! # DHT Discovery Service
//! 
//! Kademlia-based DHT for peer discovery with semantic query support.
//! Enables finding peers by:
//! - Peer ID (standard Kademlia)
//! - String ID (find nodes storing a string)
//! - Geographic zone
//! - Capability (validator, seeder, relay)

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};
use parking_lot::RwLock;
use rope_core::types::{GeoZone, StringId};

/// DHT configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DhtConfig {
    /// K-bucket size (replication factor)
    pub k: usize,
    
    /// Alpha (parallel lookups)
    pub alpha: usize,
    
    /// Bootstrap nodes
    pub bootstrap_nodes: Vec<String>,
    
    /// Refresh interval for routing table
    pub refresh_interval: Duration,
    
    /// Record TTL
    pub record_ttl: Duration,
    
    /// Enable geographic awareness
    pub geo_aware: bool,
}

impl Default for DhtConfig {
    fn default() -> Self {
        Self {
            k: 20, // Standard Kademlia K
            alpha: 3,
            bootstrap_nodes: vec![
                "/dns4/bootstrap1.datachain.one/tcp/9000".to_string(),
                "/dns4/bootstrap2.datachain.one/tcp/9000".to_string(),
            ],
            refresh_interval: Duration::from_secs(3600),
            record_ttl: Duration::from_secs(86400),
            geo_aware: true,
        }
    }
}

/// Peer capability flags
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PeerCapability {
    /// Can validate strings
    Validator,
    /// Can store and serve strings
    Seeder,
    /// Can relay messages
    Relay,
    /// Can bridge to external chains
    Bridge,
    /// Full node (all capabilities)
    FullNode,
}

/// Peer information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Peer ID (node ID)
    pub id: [u8; 32],
    
    /// Multiaddresses
    pub addresses: Vec<String>,
    
    /// Geographic zone
    pub geo_zone: Option<GeoZone>,
    
    /// Capabilities
    pub capabilities: HashSet<PeerCapability>,
    
    /// Reputation score (0-100)
    pub reputation: u8,
    
    /// Last seen timestamp
    pub last_seen: i64,
    
    /// Protocol version
    pub protocol_version: String,
}

impl PeerInfo {
    /// Create new peer info
    pub fn new(id: [u8; 32], addresses: Vec<String>) -> Self {
        Self {
            id,
            addresses,
            geo_zone: None,
            capabilities: HashSet::new(),
            reputation: 50, // Start neutral
            last_seen: chrono::Utc::now().timestamp(),
            protocol_version: "1.0.0".to_string(),
        }
    }
    
    /// Check if peer is a validator
    pub fn is_validator(&self) -> bool {
        self.capabilities.contains(&PeerCapability::Validator)
    }
    
    /// Check if peer can seed strings
    pub fn is_seeder(&self) -> bool {
        self.capabilities.contains(&PeerCapability::Seeder)
    }
    
    /// XOR distance to another peer
    pub fn distance_to(&self, other: &[u8; 32]) -> [u8; 32] {
        let mut result = [0u8; 32];
        for i in 0..32 {
            result[i] = self.id[i] ^ other[i];
        }
        result
    }
}

/// DHT record types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DhtRecord {
    /// Peer location
    PeerLocation(PeerInfo),
    
    /// String provider (who has this string)
    StringProvider {
        string_id: StringId,
        providers: Vec<[u8; 32]>,
    },
    
    /// Semantic index (for content queries)
    SemanticIndex {
        keywords: Vec<String>,
        string_ids: Vec<StringId>,
    },
}

/// K-Bucket entry
#[derive(Clone, Debug)]
struct KBucketEntry {
    peer: PeerInfo,
    last_contact: Instant,
}

/// K-Bucket for routing table
struct KBucket {
    entries: Vec<KBucketEntry>,
    k: usize,
}

impl KBucket {
    fn new(k: usize) -> Self {
        Self {
            entries: Vec::with_capacity(k),
            k,
        }
    }
    
    fn add(&mut self, peer: PeerInfo) -> bool {
        // Check if already exists
        if let Some(entry) = self.entries.iter_mut().find(|e| e.peer.id == peer.id) {
            entry.last_contact = Instant::now();
            entry.peer = peer;
            return true;
        }
        
        // Add if space available
        if self.entries.len() < self.k {
            self.entries.push(KBucketEntry {
                peer,
                last_contact: Instant::now(),
            });
            return true;
        }
        
        // Bucket full - check for stale entries
        if let Some(stale_idx) = self.entries.iter().position(|e| {
            e.last_contact.elapsed() > Duration::from_secs(3600)
        }) {
            self.entries[stale_idx] = KBucketEntry {
                peer,
                last_contact: Instant::now(),
            };
            return true;
        }
        
        false // Bucket full, no stale entries
    }
    
    fn remove(&mut self, id: &[u8; 32]) {
        self.entries.retain(|e| &e.peer.id != id);
    }
    
    fn get(&self, id: &[u8; 32]) -> Option<&PeerInfo> {
        self.entries.iter()
            .find(|e| &e.peer.id == id)
            .map(|e| &e.peer)
    }
    
    fn closest(&self, target: &[u8; 32]) -> Vec<&PeerInfo> {
        let mut sorted: Vec<_> = self.entries.iter().collect();
        sorted.sort_by(|a, b| {
            let dist_a = distance(&a.peer.id, target);
            let dist_b = distance(&b.peer.id, target);
            dist_a.cmp(&dist_b)
        });
        sorted.iter().map(|e| &e.peer).collect()
    }
}

/// Calculate XOR distance
fn distance(a: &[u8; 32], b: &[u8; 32]) -> [u8; 32] {
    let mut result = [0u8; 32];
    for i in 0..32 {
        result[i] = a[i] ^ b[i];
    }
    result
}

/// Get bucket index for a given distance
fn bucket_index(dist: &[u8; 32]) -> usize {
    for i in 0..256 {
        let byte_idx = i / 8;
        let bit_idx = 7 - (i % 8);
        if (dist[byte_idx] >> bit_idx) & 1 == 1 {
            return 255 - i;
        }
    }
    0
}

/// Discovery service
pub struct DiscoveryService {
    /// Our node ID
    node_id: [u8; 32],
    
    /// Configuration
    config: DhtConfig,
    
    /// Routing table (256 k-buckets)
    routing_table: RwLock<Vec<KBucket>>,
    
    /// Known peers (for quick lookup)
    known_peers: RwLock<HashMap<[u8; 32], PeerInfo>>,
    
    /// String providers
    providers: RwLock<HashMap<StringId, Vec<[u8; 32]>>>,
    
    /// Pending lookups
    pending_lookups: RwLock<HashSet<[u8; 32]>>,
    
    /// Statistics
    stats: RwLock<DiscoveryStats>,
}

/// Discovery statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscoveryStats {
    pub known_peers: usize,
    pub routing_table_size: usize,
    pub lookups_performed: u64,
    pub lookups_successful: u64,
    pub providers_stored: usize,
}

impl DiscoveryService {
    /// Create new discovery service
    pub fn new(node_id: [u8; 32], config: DhtConfig) -> Self {
        let k = config.k;
        let routing_table = (0..256).map(|_| KBucket::new(k)).collect();
        
        Self {
            node_id,
            config,
            routing_table: RwLock::new(routing_table),
            known_peers: RwLock::new(HashMap::new()),
            providers: RwLock::new(HashMap::new()),
            pending_lookups: RwLock::new(HashSet::new()),
            stats: RwLock::new(DiscoveryStats::default()),
        }
    }
    
    /// Add a peer to the routing table
    pub fn add_peer(&self, peer: PeerInfo) {
        let dist = distance(&self.node_id, &peer.id);
        let bucket_idx = bucket_index(&dist);
        
        let mut routing_table = self.routing_table.write();
        if routing_table[bucket_idx].add(peer.clone()) {
            self.known_peers.write().insert(peer.id, peer);
            self.update_stats();
        }
    }
    
    /// Remove a peer
    pub fn remove_peer(&self, id: &[u8; 32]) {
        let dist = distance(&self.node_id, id);
        let bucket_idx = bucket_index(&dist);
        
        self.routing_table.write()[bucket_idx].remove(id);
        self.known_peers.write().remove(id);
        self.update_stats();
    }
    
    /// Get peer by ID
    pub fn get_peer(&self, id: &[u8; 32]) -> Option<PeerInfo> {
        self.known_peers.read().get(id).cloned()
    }
    
    /// Find closest peers to a target
    pub fn find_closest(&self, target: &[u8; 32], count: usize) -> Vec<PeerInfo> {
        let routing_table = self.routing_table.read();
        let mut all_peers: Vec<&PeerInfo> = Vec::new();
        
        for bucket in routing_table.iter() {
            all_peers.extend(bucket.closest(target));
        }
        
        // Sort by distance
        all_peers.sort_by(|a, b| {
            let dist_a = distance(&a.id, target);
            let dist_b = distance(&b.id, target);
            dist_a.cmp(&dist_b)
        });
        
        all_peers.into_iter()
            .take(count)
            .cloned()
            .collect()
    }
    
    /// Find peers by capability
    pub fn find_by_capability(&self, capability: PeerCapability) -> Vec<PeerInfo> {
        self.known_peers.read()
            .values()
            .filter(|p| p.capabilities.contains(&capability))
            .cloned()
            .collect()
    }
    
    /// Find validators
    pub fn find_validators(&self) -> Vec<PeerInfo> {
        self.find_by_capability(PeerCapability::Validator)
    }
    
    /// Find peers in a geographic zone
    pub fn find_by_zone(&self, zone: GeoZone) -> Vec<PeerInfo> {
        self.known_peers.read()
            .values()
            .filter(|p| p.geo_zone == Some(zone))
            .cloned()
            .collect()
    }
    
    /// Announce as provider for a string
    pub fn announce_provider(&self, string_id: StringId) {
        self.providers.write()
            .entry(string_id)
            .or_insert_with(Vec::new)
            .push(self.node_id);
        self.update_stats();
    }
    
    /// Find providers for a string
    pub fn find_providers(&self, string_id: &StringId) -> Vec<[u8; 32]> {
        self.providers.read()
            .get(string_id)
            .cloned()
            .unwrap_or_default()
    }
    
    /// Add provider for a string
    pub fn add_provider(&self, string_id: StringId, provider_id: [u8; 32]) {
        let mut providers = self.providers.write();
        let list = providers.entry(string_id).or_insert_with(Vec::new);
        if !list.contains(&provider_id) {
            list.push(provider_id);
        }
        self.update_stats();
    }
    
    /// Select random peers for gossip
    pub fn select_gossip_peers(&self, count: usize) -> Vec<PeerInfo> {
        use rand::seq::SliceRandom;
        
        let known = self.known_peers.read();
        let mut peers: Vec<_> = known.values().cloned().collect();
        
        let mut rng = rand::thread_rng();
        peers.shuffle(&mut rng);
        
        peers.into_iter().take(count).collect()
    }
    
    /// Get all known peers
    pub fn all_peers(&self) -> Vec<PeerInfo> {
        self.known_peers.read().values().cloned().collect()
    }
    
    /// Get bootstrap nodes
    pub fn bootstrap_nodes(&self) -> &[String] {
        &self.config.bootstrap_nodes
    }
    
    /// Update statistics
    fn update_stats(&self) {
        let mut stats = self.stats.write();
        stats.known_peers = self.known_peers.read().len();
        stats.providers_stored = self.providers.read().len();
        stats.routing_table_size = self.routing_table.read()
            .iter()
            .map(|b| b.entries.len())
            .sum();
    }
    
    /// Get statistics
    pub fn stats(&self) -> DiscoveryStats {
        self.stats.read().clone()
    }
}

impl Default for DiscoveryService {
    fn default() -> Self {
        Self::new([0u8; 32], DhtConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_dht_config() {
        let config = DhtConfig::default();
        assert_eq!(config.k, 20);
        assert_eq!(config.alpha, 3);
    }
    
    #[test]
    fn test_peer_info() {
        let mut peer = PeerInfo::new([1u8; 32], vec!["/ip4/127.0.0.1/tcp/9000".to_string()]);
        peer.capabilities.insert(PeerCapability::Validator);
        
        assert!(peer.is_validator());
        assert!(!peer.is_seeder());
    }
    
    #[test]
    fn test_distance() {
        let a = [0u8; 32];
        let b = [1u8; 32];
        let dist = distance(&a, &b);
        assert_eq!(dist[0], 1);
    }
    
    #[test]
    fn test_discovery_service() {
        let service = DiscoveryService::new([0u8; 32], DhtConfig::default());
        
        // Add peers
        for i in 1..=10 {
            let mut peer = PeerInfo::new([i as u8; 32], vec![]);
            if i % 2 == 0 {
                peer.capabilities.insert(PeerCapability::Validator);
            }
            service.add_peer(peer);
        }
        
        // Find closest
        let closest = service.find_closest(&[5u8; 32], 3);
        assert_eq!(closest.len(), 3);
        
        // Find validators
        let validators = service.find_validators();
        assert_eq!(validators.len(), 5);
    }
    
    #[test]
    fn test_provider_announcement() {
        let service = DiscoveryService::new([0u8; 32], DhtConfig::default());
        let string_id = StringId::from_content(b"test");
        
        service.announce_provider(string_id);
        
        let providers = service.find_providers(&string_id);
        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0], [0u8; 32]);
    }
}

