//! # Peer Management
//!
//! Manages peer connections, states, and reputation.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

/// Peer ID (32-byte node identifier)
pub type PeerId = [u8; 32];

/// Peer connection state
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PeerState {
    /// Not connected
    Disconnected,
    /// Connection in progress
    Connecting,
    /// Connected and ready
    Connected,
    /// Temporarily banned
    Banned { until: i64, reason: String },
}

/// Peer connection info
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PeerConnection {
    /// Peer ID
    pub id: PeerId,

    /// Current state
    pub state: PeerState,

    /// Multiaddress
    pub address: String,

    /// Connected at
    pub connected_at: Option<i64>,

    /// Last activity
    pub last_activity: i64,

    /// Reputation score (0-100)
    pub reputation: u8,

    /// Latency in milliseconds
    pub latency_ms: Option<u32>,

    /// Messages sent
    pub messages_sent: u64,

    /// Messages received
    pub messages_received: u64,

    /// Bytes sent
    pub bytes_sent: u64,

    /// Bytes received
    pub bytes_received: u64,
}

impl PeerConnection {
    /// Create new peer connection
    pub fn new(id: PeerId, address: String) -> Self {
        Self {
            id,
            state: PeerState::Disconnected,
            address,
            connected_at: None,
            last_activity: chrono::Utc::now().timestamp(),
            reputation: 50, // Neutral starting reputation
            latency_ms: None,
            messages_sent: 0,
            messages_received: 0,
            bytes_sent: 0,
            bytes_received: 0,
        }
    }

    /// Mark as connected
    pub fn mark_connected(&mut self) {
        self.state = PeerState::Connected;
        self.connected_at = Some(chrono::Utc::now().timestamp());
        self.last_activity = chrono::Utc::now().timestamp();
    }

    /// Mark as disconnected
    pub fn mark_disconnected(&mut self) {
        self.state = PeerState::Disconnected;
    }

    /// Ban the peer
    pub fn ban(&mut self, duration: Duration, reason: String) {
        let until = chrono::Utc::now().timestamp() + duration.as_secs() as i64;
        self.state = PeerState::Banned { until, reason };
    }

    /// Check if banned
    pub fn is_banned(&self) -> bool {
        if let PeerState::Banned { until, .. } = &self.state {
            chrono::Utc::now().timestamp() < *until
        } else {
            false
        }
    }

    /// Update reputation
    pub fn update_reputation(&mut self, delta: i8) {
        let new_rep = (self.reputation as i16 + delta as i16).clamp(0, 100) as u8;
        self.reputation = new_rep;
    }

    /// Record sent data
    pub fn record_sent(&mut self, bytes: u64) {
        self.messages_sent += 1;
        self.bytes_sent += bytes;
        self.last_activity = chrono::Utc::now().timestamp();
    }

    /// Record received data
    pub fn record_received(&mut self, bytes: u64) {
        self.messages_received += 1;
        self.bytes_received += bytes;
        self.last_activity = chrono::Utc::now().timestamp();
    }
}

/// Peer manager
pub struct PeerManager {
    /// Our node ID
    node_id: PeerId,

    /// All peers
    peers: RwLock<HashMap<PeerId, PeerConnection>>,

    /// Connected peers
    connected: RwLock<Vec<PeerId>>,

    /// Maximum connections
    max_connections: usize,

    /// Connection timeout
    connection_timeout: Duration,

    /// Statistics
    stats: RwLock<PeerManagerStats>,
}

/// Peer manager statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PeerManagerStats {
    pub total_peers: usize,
    pub connected_peers: usize,
    pub banned_peers: usize,
    pub avg_reputation: f64,
    pub avg_latency_ms: f64,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
}

impl PeerManager {
    /// Create new peer manager
    pub fn new(node_id: PeerId, max_connections: usize) -> Self {
        Self {
            node_id,
            peers: RwLock::new(HashMap::new()),
            connected: RwLock::new(Vec::new()),
            max_connections,
            connection_timeout: Duration::from_secs(30),
            stats: RwLock::new(PeerManagerStats::default()),
        }
    }

    /// Add a peer
    pub fn add_peer(&self, peer: PeerConnection) {
        let id = peer.id;
        self.peers.write().insert(id, peer);
        self.update_stats();
    }

    /// Get peer by ID
    pub fn get_peer(&self, id: &PeerId) -> Option<PeerConnection> {
        self.peers.read().get(id).cloned()
    }

    /// Connect to a peer
    pub fn connect(&self, id: &PeerId) -> Result<(), PeerError> {
        let mut peers = self.peers.write();
        let peer = peers.get_mut(id).ok_or(PeerError::NotFound)?;

        if peer.is_banned() {
            return Err(PeerError::Banned);
        }

        if self.connected.read().len() >= self.max_connections {
            return Err(PeerError::TooManyConnections);
        }

        peer.mark_connected();
        self.connected.write().push(*id);
        self.update_stats();

        Ok(())
    }

    /// Disconnect from a peer
    pub fn disconnect(&self, id: &PeerId) {
        if let Some(peer) = self.peers.write().get_mut(id) {
            peer.mark_disconnected();
        }
        self.connected.write().retain(|&pid| pid != *id);
        self.update_stats();
    }

    /// Ban a peer
    pub fn ban(&self, id: &PeerId, duration: Duration, reason: String) {
        if let Some(peer) = self.peers.write().get_mut(id) {
            peer.ban(duration, reason);
        }
        self.disconnect(id);
        self.update_stats();
    }

    /// Get connected peers
    pub fn connected_peers(&self) -> Vec<PeerId> {
        self.connected.read().clone()
    }

    /// Get peers by reputation
    pub fn peers_by_reputation(&self, min_reputation: u8) -> Vec<PeerConnection> {
        self.peers
            .read()
            .values()
            .filter(|p| p.reputation >= min_reputation)
            .cloned()
            .collect()
    }

    /// Select random connected peer
    pub fn random_peer(&self) -> Option<PeerId> {
        use rand::seq::SliceRandom;

        let connected = self.connected.read();
        let mut rng = rand::thread_rng();
        connected.choose(&mut rng).copied()
    }

    /// Prune stale connections
    pub fn prune_stale(&self, max_idle: Duration) {
        let now = chrono::Utc::now().timestamp();
        let max_idle_secs = max_idle.as_secs() as i64;

        let stale: Vec<PeerId> = {
            let peers = self.peers.read();
            let connected = self.connected.read();

            connected
                .iter()
                .filter(|id| {
                    peers
                        .get(*id)
                        .map(|p| now - p.last_activity > max_idle_secs)
                        .unwrap_or(true)
                })
                .copied()
                .collect()
        };

        for id in stale {
            self.disconnect(&id);
        }
    }

    /// Update statistics
    fn update_stats(&self) {
        let peers = self.peers.read();
        let connected = self.connected.read();

        let mut stats = self.stats.write();
        stats.total_peers = peers.len();
        stats.connected_peers = connected.len();
        stats.banned_peers = peers.values().filter(|p| p.is_banned()).count();

        if !peers.is_empty() {
            stats.avg_reputation =
                peers.values().map(|p| p.reputation as f64).sum::<f64>() / peers.len() as f64;

            let latencies: Vec<f64> = peers
                .values()
                .filter_map(|p| p.latency_ms.map(|l| l as f64))
                .collect();

            if !latencies.is_empty() {
                stats.avg_latency_ms = latencies.iter().sum::<f64>() / latencies.len() as f64;
            }
        }

        stats.total_bytes_sent = peers.values().map(|p| p.bytes_sent).sum();
        stats.total_bytes_received = peers.values().map(|p| p.bytes_received).sum();
    }

    /// Get statistics
    pub fn stats(&self) -> PeerManagerStats {
        self.stats.read().clone()
    }

    /// Get our node ID
    pub fn node_id(&self) -> PeerId {
        self.node_id
    }
}

impl Default for PeerManager {
    fn default() -> Self {
        Self::new([0u8; 32], 100)
    }
}

/// Peer errors
#[derive(Clone, Debug)]
pub enum PeerError {
    NotFound,
    Banned,
    TooManyConnections,
    ConnectionFailed,
    Timeout,
}

impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PeerError::NotFound => write!(f, "Peer not found"),
            PeerError::Banned => write!(f, "Peer is banned"),
            PeerError::TooManyConnections => write!(f, "Too many connections"),
            PeerError::ConnectionFailed => write!(f, "Connection failed"),
            PeerError::Timeout => write!(f, "Connection timeout"),
        }
    }
}

impl std::error::Error for PeerError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_peer_connection() {
        let mut peer = PeerConnection::new([1u8; 32], "/ip4/127.0.0.1/tcp/9000".to_string());

        assert_eq!(peer.state, PeerState::Disconnected);
        assert_eq!(peer.reputation, 50);

        peer.mark_connected();
        assert_eq!(peer.state, PeerState::Connected);

        peer.record_sent(1000);
        assert_eq!(peer.bytes_sent, 1000);
        assert_eq!(peer.messages_sent, 1);
    }

    #[test]
    fn test_reputation() {
        let mut peer = PeerConnection::new([1u8; 32], "".to_string());

        peer.update_reputation(10);
        assert_eq!(peer.reputation, 60);

        peer.update_reputation(-100);
        assert_eq!(peer.reputation, 0);

        peer.update_reputation(100);
        assert_eq!(peer.reputation, 100);
    }

    #[test]
    fn test_ban() {
        let mut peer = PeerConnection::new([1u8; 32], "".to_string());

        peer.ban(Duration::from_secs(3600), "Misbehavior".to_string());
        assert!(peer.is_banned());
    }

    #[test]
    fn test_peer_manager() {
        let manager = PeerManager::new([0u8; 32], 10);

        // Add peers
        for i in 1..=5 {
            let peer = PeerConnection::new([i as u8; 32], format!("/ip4/127.0.0.{}/tcp/9000", i));
            manager.add_peer(peer);
        }

        // Connect to some
        assert!(manager.connect(&[1u8; 32]).is_ok());
        assert!(manager.connect(&[2u8; 32]).is_ok());

        let stats = manager.stats();
        assert_eq!(stats.total_peers, 5);
        assert_eq!(stats.connected_peers, 2);
    }
}
