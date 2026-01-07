//! # Rope Distribution Protocol (RDP)
//! 
//! BitTorrent-inspired protocol for efficient string distribution.
//! 
//! ## Key Features
//! 
//! - Piece-based distribution (256KB pieces)
//! - Rarest-first strategy
//! - Swarm coordination
//! - Incentive-compatible seeding
//! 
//! ## Protocol Flow
//! 
//! 1. Client requests string by ID
//! 2. DHT lookup finds providers (seeders)
//! 3. Client joins swarm for that string
//! 4. Pieces are requested using rarest-first
//! 5. Complete string is verified against StringId
//! 6. Client becomes seeder

use std::collections::{HashMap, HashSet, VecDeque};
use std::time::Duration;
use serde::{Deserialize, Serialize};
use parking_lot::RwLock;
use rope_core::types::StringId;

/// RDP configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RdpConfig {
    /// Piece size in bytes (default 256KB)
    pub piece_size: usize,
    
    /// Maximum concurrent downloads
    pub max_concurrent_downloads: usize,
    
    /// Maximum upload slots per swarm
    pub max_upload_slots: usize,
    
    /// Minimum seeding ratio
    pub min_seed_ratio: f64,
    
    /// Request timeout
    pub request_timeout: Duration,
    
    /// Enable encryption
    pub enable_encryption: bool,
}

impl Default for RdpConfig {
    fn default() -> Self {
        Self {
            piece_size: 256 * 1024, // 256KB
            max_concurrent_downloads: 10,
            max_upload_slots: 20,
            min_seed_ratio: 1.0,
            request_timeout: Duration::from_secs(30),
            enable_encryption: true,
        }
    }
}

/// Piece state
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PieceState {
    /// Not yet requested
    Missing,
    /// Currently being downloaded
    Downloading { from: [u8; 32], started_at: i64 },
    /// Downloaded but not verified
    Downloaded,
    /// Verified and complete
    Complete,
}

/// Piece information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PieceInfo {
    /// Piece index
    pub index: u32,
    
    /// Piece hash
    pub hash: [u8; 32],
    
    /// Piece size (may be smaller for last piece)
    pub size: usize,
    
    /// Current state
    pub state: PieceState,
    
    /// Data (if downloaded)
    #[serde(skip)]
    pub data: Option<Vec<u8>>,
}

/// String metadata for distribution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StringMetadata {
    /// String ID
    pub string_id: StringId,
    
    /// Total size in bytes
    pub total_size: usize,
    
    /// Number of pieces
    pub piece_count: u32,
    
    /// Piece hashes
    pub piece_hashes: Vec<[u8; 32]>,
    
    /// Creation timestamp
    pub created_at: i64,
    
    /// Creator node ID
    pub creator: [u8; 32],
}

/// Swarm member
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SwarmMember {
    /// Node ID
    pub node_id: [u8; 32],
    
    /// Pieces this member has
    pub have_pieces: HashSet<u32>,
    
    /// Is this member a seeder (has all pieces)?
    pub is_seeder: bool,
    
    /// Download rate (bytes/sec)
    pub download_rate: u64,
    
    /// Upload rate (bytes/sec)
    pub upload_rate: u64,
    
    /// Last seen timestamp
    pub last_seen: i64,
}

/// Swarm for a string
pub struct Swarm {
    /// String ID
    string_id: StringId,
    
    /// String metadata
    metadata: StringMetadata,
    
    /// Our pieces
    pieces: RwLock<Vec<PieceInfo>>,
    
    /// Swarm members
    members: RwLock<HashMap<[u8; 32], SwarmMember>>,
    
    /// Piece availability (which members have which pieces)
    piece_availability: RwLock<HashMap<u32, HashSet<[u8; 32]>>>,
    
    /// Download queue (rarest-first ordered)
    download_queue: RwLock<VecDeque<u32>>,
    
    /// Statistics
    stats: RwLock<SwarmStats>,
}

/// Swarm statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SwarmStats {
    pub total_members: usize,
    pub seeders: usize,
    pub leechers: usize,
    pub pieces_downloaded: u32,
    pub pieces_uploaded: u32,
    pub bytes_downloaded: u64,
    pub bytes_uploaded: u64,
    pub completion_percentage: f64,
}

impl Swarm {
    /// Create new swarm
    pub fn new(metadata: StringMetadata) -> Self {
        let pieces = (0..metadata.piece_count)
            .map(|i| PieceInfo {
                index: i,
                hash: metadata.piece_hashes.get(i as usize).copied().unwrap_or([0u8; 32]),
                size: if i == metadata.piece_count - 1 {
                    metadata.total_size % 262144 // Last piece may be smaller
                } else {
                    262144 // 256KB
                },
                state: PieceState::Missing,
                data: None,
            })
            .collect();
        
        Self {
            string_id: metadata.string_id,
            metadata,
            pieces: RwLock::new(pieces),
            members: RwLock::new(HashMap::new()),
            piece_availability: RwLock::new(HashMap::new()),
            download_queue: RwLock::new(VecDeque::new()),
            stats: RwLock::new(SwarmStats::default()),
        }
    }
    
    /// Add a member to the swarm
    pub fn add_member(&self, member: SwarmMember) {
        let node_id = member.node_id;
        let is_seeder = member.is_seeder;
        let have_pieces = member.have_pieces.clone();
        
        self.members.write().insert(node_id, member);
        
        // Update piece availability
        let mut availability = self.piece_availability.write();
        for piece_idx in have_pieces {
            availability
                .entry(piece_idx)
                .or_insert_with(HashSet::new)
                .insert(node_id);
        }
        
        // Update stats
        let mut stats = self.stats.write();
        stats.total_members = self.members.read().len();
        if is_seeder {
            stats.seeders += 1;
        } else {
            stats.leechers += 1;
        }
        
        // Recompute download queue (rarest-first)
        self.recompute_download_queue();
    }
    
    /// Remove a member from the swarm
    pub fn remove_member(&self, node_id: &[u8; 32]) {
        if let Some(member) = self.members.write().remove(node_id) {
            // Remove from piece availability
            let mut availability = self.piece_availability.write();
            for piece_idx in member.have_pieces {
                if let Some(nodes) = availability.get_mut(&piece_idx) {
                    nodes.remove(node_id);
                }
            }
            
            // Update stats
            let mut stats = self.stats.write();
            stats.total_members = self.members.read().len();
            if member.is_seeder {
                stats.seeders = stats.seeders.saturating_sub(1);
            } else {
                stats.leechers = stats.leechers.saturating_sub(1);
            }
        }
    }
    
    /// Recompute download queue using rarest-first strategy
    fn recompute_download_queue(&self) {
        let pieces = self.pieces.read();
        let availability = self.piece_availability.read();
        
        // Get missing pieces with their rarity
        let mut missing: Vec<(u32, usize)> = pieces.iter()
            .filter(|p| p.state == PieceState::Missing)
            .map(|p| {
                let rarity = availability.get(&p.index)
                    .map(|nodes| nodes.len())
                    .unwrap_or(0);
                (p.index, rarity)
            })
            .collect();
        
        // Sort by rarity (ascending - rarest first)
        missing.sort_by_key(|(_, rarity)| *rarity);
        
        // Update queue
        let mut queue = self.download_queue.write();
        queue.clear();
        for (idx, _) in missing {
            queue.push_back(idx);
        }
    }
    
    /// Get next piece to download
    pub fn next_piece_to_download(&self) -> Option<(u32, [u8; 32])> {
        let queue = self.download_queue.read();
        let availability = self.piece_availability.read();
        
        for &piece_idx in queue.iter() {
            if let Some(nodes) = availability.get(&piece_idx) {
                if let Some(&node_id) = nodes.iter().next() {
                    return Some((piece_idx, node_id));
                }
            }
        }
        
        None
    }
    
    /// Mark piece as downloading
    pub fn mark_downloading(&self, piece_idx: u32, from: [u8; 32]) {
        let mut pieces = self.pieces.write();
        if let Some(piece) = pieces.get_mut(piece_idx as usize) {
            piece.state = PieceState::Downloading {
                from,
                started_at: chrono::Utc::now().timestamp(),
            };
        }
        
        // Remove from queue
        self.download_queue.write().retain(|&idx| idx != piece_idx);
    }
    
    /// Receive piece data
    pub fn receive_piece(&self, piece_idx: u32, data: Vec<u8>) -> bool {
        let mut pieces = self.pieces.write();
        if let Some(piece) = pieces.get_mut(piece_idx as usize) {
            // Verify hash
            let hash = *blake3::hash(&data).as_bytes();
            if hash != piece.hash {
                // Hash mismatch - mark as missing again
                piece.state = PieceState::Missing;
                self.download_queue.write().push_back(piece_idx);
                return false;
            }
            
            piece.data = Some(data.clone());
            piece.state = PieceState::Complete;
            
            // Update stats
            let mut stats = self.stats.write();
            stats.pieces_downloaded += 1;
            stats.bytes_downloaded += data.len() as u64;
            
            // Update completion percentage
            let complete = pieces.iter().filter(|p| p.state == PieceState::Complete).count();
            stats.completion_percentage = complete as f64 / pieces.len() as f64 * 100.0;
            
            return true;
        }
        false
    }
    
    /// Check if download is complete
    pub fn is_complete(&self) -> bool {
        self.pieces.read().iter().all(|p| p.state == PieceState::Complete)
    }
    
    /// Get complete string data
    pub fn get_complete_data(&self) -> Option<Vec<u8>> {
        if !self.is_complete() {
            return None;
        }
        
        let pieces = self.pieces.read();
        let mut data = Vec::with_capacity(self.metadata.total_size);
        
        for piece in pieces.iter() {
            if let Some(ref piece_data) = piece.data {
                data.extend_from_slice(piece_data);
            } else {
                return None;
            }
        }
        
        Some(data)
    }
    
    /// Get statistics
    pub fn stats(&self) -> SwarmStats {
        self.stats.read().clone()
    }
    
    /// Get string ID
    pub fn string_id(&self) -> StringId {
        self.string_id
    }
}

/// RDP message types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RdpMessage {
    /// Request to join swarm
    Join { string_id: StringId },
    
    /// Announce pieces we have
    Have { pieces: Vec<u32> },
    
    /// Request a piece
    Request { piece_idx: u32 },
    
    /// Piece data
    Piece { piece_idx: u32, data: Vec<u8> },
    
    /// Cancel a request
    Cancel { piece_idx: u32 },
    
    /// Keep-alive
    KeepAlive,
    
    /// Choke (stop uploading to peer)
    Choke,
    
    /// Unchoke (allow uploads to peer)
    Unchoke,
}

/// Rope Distribution Protocol manager
pub struct RopeDistributionProtocol {
    /// Configuration
    config: RdpConfig,
    
    /// Our node ID
    node_id: [u8; 32],
    
    /// Active swarms
    swarms: RwLock<HashMap<StringId, Swarm>>,
    
    /// Download history (for ratio calculation)
    download_history: RwLock<HashMap<StringId, u64>>,
    
    /// Upload history
    upload_history: RwLock<HashMap<StringId, u64>>,
    
    /// Statistics
    stats: RwLock<RdpStats>,
}

/// RDP statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RdpStats {
    pub active_swarms: usize,
    pub total_downloaded: u64,
    pub total_uploaded: u64,
    pub current_download_rate: u64,
    pub current_upload_rate: u64,
    pub seed_ratio: f64,
}

impl RopeDistributionProtocol {
    /// Create new RDP
    pub fn new(node_id: [u8; 32], config: RdpConfig) -> Self {
        Self {
            config,
            node_id,
            swarms: RwLock::new(HashMap::new()),
            download_history: RwLock::new(HashMap::new()),
            upload_history: RwLock::new(HashMap::new()),
            stats: RwLock::new(RdpStats::default()),
        }
    }
    
    /// Start downloading a string
    pub fn start_download(&self, metadata: StringMetadata) -> StringId {
        let string_id = metadata.string_id;
        let swarm = Swarm::new(metadata);
        
        self.swarms.write().insert(string_id, swarm);
        self.update_stats();
        
        string_id
    }
    
    /// Join an existing swarm as a seeder
    pub fn join_as_seeder(&self, metadata: StringMetadata, data: Vec<u8>) {
        let string_id = metadata.string_id;
        let piece_count = metadata.piece_count;
        let swarm = Swarm::new(metadata);
        
        // Mark all pieces as complete
        {
            let mut pieces = swarm.pieces.write();
            let piece_size = self.config.piece_size;
            
            for (i, piece) in pieces.iter_mut().enumerate() {
                let start = i * piece_size;
                let end = (start + piece_size).min(data.len());
                piece.data = Some(data[start..end].to_vec());
                piece.state = PieceState::Complete;
            }
        }
        
        // Add ourselves as seeder
        let member = SwarmMember {
            node_id: self.node_id,
            have_pieces: (0..piece_count).collect(),
            is_seeder: true,
            download_rate: 0,
            upload_rate: 0,
            last_seen: chrono::Utc::now().timestamp(),
        };
        swarm.add_member(member);
        
        self.swarms.write().insert(string_id, swarm);
        self.update_stats();
    }
    
    /// Handle incoming RDP message
    pub fn handle_message(&self, string_id: &StringId, from: [u8; 32], msg: RdpMessage) -> Option<RdpMessage> {
        let swarms = self.swarms.read();
        let swarm = swarms.get(string_id)?;
        
        match msg {
            RdpMessage::Join { string_id: _ } => {
                // New peer joined
                let member = SwarmMember {
                    node_id: from,
                    have_pieces: HashSet::new(),
                    is_seeder: false,
                    download_rate: 0,
                    upload_rate: 0,
                    last_seen: chrono::Utc::now().timestamp(),
                };
                
                drop(swarms);
                self.swarms.read().get(string_id)?.add_member(member);
                
                // Send our pieces
                let pieces: Vec<u32> = self.swarms.read()
                    .get(string_id)?
                    .pieces.read()
                    .iter()
                    .filter(|p| p.state == PieceState::Complete)
                    .map(|p| p.index)
                    .collect();
                
                Some(RdpMessage::Have { pieces })
            }
            
            RdpMessage::Have { pieces } => {
                // Update member's pieces
                if let Some(member) = self.swarms.read()
                    .get(string_id)?
                    .members.write()
                    .get_mut(&from) 
                {
                    member.have_pieces.extend(pieces);
                    member.is_seeder = member.have_pieces.len() as u32 == 
                        self.swarms.read().get(string_id)?.metadata.piece_count;
                }
                None
            }
            
            RdpMessage::Request { piece_idx } => {
                // Send piece if we have it
                let piece_data = {
                    let pieces = swarm.pieces.read();
                    pieces.get(piece_idx as usize)
                        .and_then(|p| p.data.clone())
                };
                
                piece_data.map(|data| {
                    self.upload_history.write()
                        .entry(*string_id)
                        .and_modify(|v| *v += data.len() as u64)
                        .or_insert(data.len() as u64);
                    self.update_stats();
                    RdpMessage::Piece { piece_idx, data }
                })
            }
            
            RdpMessage::Piece { piece_idx, data } => {
                // Receive piece
                drop(swarms);
                if let Some(swarm) = self.swarms.read().get(string_id) {
                    swarm.receive_piece(piece_idx, data.clone());
                    
                    self.download_history.write()
                        .entry(*string_id)
                        .and_modify(|v| *v += data.len() as u64)
                        .or_insert(data.len() as u64);
                }
                self.update_stats();
                None
            }
            
            RdpMessage::Cancel { piece_idx: _ } => {
                // Cancel request (ignore for now)
                None
            }
            
            RdpMessage::KeepAlive => {
                // Update last seen
                if let Some(member) = self.swarms.read()
                    .get(string_id)?
                    .members.write()
                    .get_mut(&from) 
                {
                    member.last_seen = chrono::Utc::now().timestamp();
                }
                None
            }
            
            RdpMessage::Choke | RdpMessage::Unchoke => {
                // Handle choking (for future rate limiting)
                None
            }
        }
    }
    
    /// Get swarm for a string
    pub fn get_swarm(&self, string_id: &StringId) -> Option<SwarmStats> {
        self.swarms.read().get(string_id).map(|s| s.stats())
    }
    
    /// Check if download is complete
    pub fn is_complete(&self, string_id: &StringId) -> bool {
        self.swarms.read()
            .get(string_id)
            .map(|s| s.is_complete())
            .unwrap_or(false)
    }
    
    /// Get complete data
    pub fn get_data(&self, string_id: &StringId) -> Option<Vec<u8>> {
        self.swarms.read()
            .get(string_id)
            .and_then(|s| s.get_complete_data())
    }
    
    /// Update statistics
    fn update_stats(&self) {
        let mut stats = self.stats.write();
        stats.active_swarms = self.swarms.read().len();
        
        let downloaded: u64 = self.download_history.read().values().sum();
        let uploaded: u64 = self.upload_history.read().values().sum();
        
        stats.total_downloaded = downloaded;
        stats.total_uploaded = uploaded;
        
        if downloaded > 0 {
            stats.seed_ratio = uploaded as f64 / downloaded as f64;
        }
    }
    
    /// Get statistics
    pub fn stats(&self) -> RdpStats {
        self.stats.read().clone()
    }
}

impl Default for RopeDistributionProtocol {
    fn default() -> Self {
        Self::new([0u8; 32], RdpConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_rdp_config() {
        let config = RdpConfig::default();
        assert_eq!(config.piece_size, 256 * 1024);
        assert_eq!(config.min_seed_ratio, 1.0);
    }
    
    #[test]
    fn test_swarm_creation() {
        let metadata = StringMetadata {
            string_id: StringId::from_content(b"test"),
            total_size: 1024 * 1024, // 1MB
            piece_count: 4,
            piece_hashes: vec![[1u8; 32], [2u8; 32], [3u8; 32], [4u8; 32]],
            created_at: 0,
            creator: [0u8; 32],
        };
        
        let swarm = Swarm::new(metadata);
        let stats = swarm.stats();
        
        assert_eq!(stats.total_members, 0);
        assert_eq!(stats.completion_percentage, 0.0);
    }
    
    #[test]
    fn test_swarm_members() {
        let metadata = StringMetadata {
            string_id: StringId::from_content(b"test"),
            total_size: 1024,
            piece_count: 2,
            piece_hashes: vec![[1u8; 32], [2u8; 32]],
            created_at: 0,
            creator: [0u8; 32],
        };
        
        let swarm = Swarm::new(metadata);
        
        // Add seeder
        let seeder = SwarmMember {
            node_id: [1u8; 32],
            have_pieces: vec![0, 1].into_iter().collect(),
            is_seeder: true,
            download_rate: 0,
            upload_rate: 0,
            last_seen: chrono::Utc::now().timestamp(),
        };
        swarm.add_member(seeder);
        
        // Add leecher
        let leecher = SwarmMember {
            node_id: [2u8; 32],
            have_pieces: vec![0].into_iter().collect(),
            is_seeder: false,
            download_rate: 0,
            upload_rate: 0,
            last_seen: chrono::Utc::now().timestamp(),
        };
        swarm.add_member(leecher);
        
        let stats = swarm.stats();
        assert_eq!(stats.total_members, 2);
        assert_eq!(stats.seeders, 1);
        assert_eq!(stats.leechers, 1);
    }
    
    #[test]
    fn test_rarest_first() {
        let metadata = StringMetadata {
            string_id: StringId::from_content(b"test"),
            total_size: 1024,
            piece_count: 3,
            piece_hashes: vec![[1u8; 32], [2u8; 32], [3u8; 32]],
            created_at: 0,
            creator: [0u8; 32],
        };
        
        let swarm = Swarm::new(metadata);
        
        // Peer 1 has pieces 0, 1
        let peer1 = SwarmMember {
            node_id: [1u8; 32],
            have_pieces: vec![0, 1].into_iter().collect(),
            is_seeder: false,
            download_rate: 0,
            upload_rate: 0,
            last_seen: chrono::Utc::now().timestamp(),
        };
        swarm.add_member(peer1);
        
        // Peer 2 has pieces 0, 2
        let peer2 = SwarmMember {
            node_id: [2u8; 32],
            have_pieces: vec![0, 2].into_iter().collect(),
            is_seeder: false,
            download_rate: 0,
            upload_rate: 0,
            last_seen: chrono::Utc::now().timestamp(),
        };
        swarm.add_member(peer2);
        
        // Piece 1 and 2 are rarest (only 1 peer each), piece 0 is most common (2 peers)
        // So download queue should prioritize piece 1 or 2
        let (piece_idx, _) = swarm.next_piece_to_download().unwrap();
        assert!(piece_idx == 1 || piece_idx == 2);
    }
}

