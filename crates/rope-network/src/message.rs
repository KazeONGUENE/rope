//! # Network Messages
//! 
//! Defines all network message types for the Datachain Rope protocol.

use serde::{Deserialize, Serialize};
use rope_core::types::StringId;

/// Message type identifier
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MessageType {
    // === Gossip Protocol ===
    /// Announce available strings
    GossipHave,
    /// Request strings
    GossipWant,
    /// String data
    GossipData,
    
    // === DHT Protocol ===
    /// Find node
    DhtFindNode,
    /// Node found response
    DhtNodeFound,
    /// Find value
    DhtFindValue,
    /// Value found response
    DhtValueFound,
    /// Store value
    DhtStore,
    
    // === RDP Protocol ===
    /// Join swarm
    RdpJoin,
    /// Leave swarm
    RdpLeave,
    /// Piece availability
    RdpHave,
    /// Request piece
    RdpRequest,
    /// Piece data
    RdpPiece,
    
    // === Consensus ===
    /// Testimony broadcast
    Testimony,
    /// Anchor string
    Anchor,
    
    // === Control ===
    /// Ping
    Ping,
    /// Pong
    Pong,
    /// Handshake
    Handshake,
    /// Disconnect
    Disconnect,
}

/// Network message envelope
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkMessage {
    /// Message ID
    pub id: [u8; 32],
    
    /// Message type
    pub message_type: MessageType,
    
    /// Sender node ID
    pub sender: [u8; 32],
    
    /// Target node ID (None for broadcast)
    pub target: Option<[u8; 32]>,
    
    /// Payload
    pub payload: Vec<u8>,
    
    /// Timestamp
    pub timestamp: i64,
    
    /// TTL (hops remaining)
    pub ttl: u8,
    
    /// Signature
    pub signature: Vec<u8>,
}

impl NetworkMessage {
    /// Create new message
    pub fn new(
        sender: [u8; 32],
        message_type: MessageType,
        payload: Vec<u8>,
    ) -> Self {
        let timestamp = chrono::Utc::now().timestamp();
        
        let mut id_data = sender.to_vec();
        id_data.extend_from_slice(&timestamp.to_le_bytes());
        id_data.extend_from_slice(&payload);
        let id = *blake3::hash(&id_data).as_bytes();
        
        Self {
            id,
            message_type,
            sender,
            target: None,
            payload,
            timestamp,
            ttl: 10,
            signature: Vec::new(),
        }
    }
    
    /// Create targeted message
    pub fn new_targeted(
        sender: [u8; 32],
        target: [u8; 32],
        message_type: MessageType,
        payload: Vec<u8>,
    ) -> Self {
        let mut msg = Self::new(sender, message_type, payload);
        msg.target = Some(target);
        msg
    }
    
    /// Create ping message
    pub fn ping(sender: [u8; 32]) -> Self {
        Self::new(sender, MessageType::Ping, Vec::new())
    }
    
    /// Create pong message
    pub fn pong(sender: [u8; 32], ping_id: [u8; 32]) -> Self {
        Self::new(sender, MessageType::Pong, ping_id.to_vec())
    }
    
    /// Create handshake message
    pub fn handshake(sender: [u8; 32], protocol_version: &str) -> Self {
        Self::new(sender, MessageType::Handshake, protocol_version.as_bytes().to_vec())
    }
    
    /// Set signature
    pub fn set_signature(&mut self, signature: Vec<u8>) {
        self.signature = signature;
    }
    
    /// Check if message is expired
    pub fn is_expired(&self, max_age_secs: i64) -> bool {
        let now = chrono::Utc::now().timestamp();
        now - self.timestamp > max_age_secs
    }
    
    /// Decrement TTL
    pub fn decrement_ttl(&mut self) -> bool {
        if self.ttl > 0 {
            self.ttl -= 1;
            true
        } else {
            false
        }
    }
    
    /// Get signing data
    pub fn signing_data(&self) -> Vec<u8> {
        let mut data = self.id.to_vec();
        data.push(self.message_type as u8);
        data.extend_from_slice(&self.sender);
        data.extend_from_slice(&self.timestamp.to_le_bytes());
        data.extend_from_slice(&self.payload);
        data
    }
}

/// Handshake data
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HandshakeData {
    /// Protocol version
    pub protocol_version: String,
    
    /// Node capabilities
    pub capabilities: Vec<String>,
    
    /// Genesis string hash
    pub genesis_hash: [u8; 32],
    
    /// Current head string ID
    pub head_string: StringId,
    
    /// User agent
    pub user_agent: String,
}

impl HandshakeData {
    /// Create new handshake data
    pub fn new(protocol_version: String, genesis_hash: [u8; 32]) -> Self {
        Self {
            protocol_version,
            capabilities: vec!["gossip".to_string(), "rdp".to_string(), "dht".to_string()],
            genesis_hash,
            head_string: StringId::default(),
            user_agent: format!("datachain-rope/{}", env!("CARGO_PKG_VERSION")),
        }
    }
    
    /// Serialize to bytes
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap_or_default()
    }
    
    /// Deserialize from bytes
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        bincode::deserialize(data).ok()
    }
}

/// Ping data
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PingData {
    pub nonce: u64,
    pub timestamp: i64,
}

impl PingData {
    pub fn new() -> Self {
        Self {
            nonce: rand::random(),
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

impl Default for PingData {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_message_creation() {
        let msg = NetworkMessage::new(
            [1u8; 32],
            MessageType::Ping,
            Vec::new(),
        );
        
        assert_eq!(msg.sender, [1u8; 32]);
        assert_eq!(msg.message_type, MessageType::Ping);
        assert_eq!(msg.ttl, 10);
    }
    
    #[test]
    fn test_targeted_message() {
        let msg = NetworkMessage::new_targeted(
            [1u8; 32],
            [2u8; 32],
            MessageType::GossipData,
            b"test data".to_vec(),
        );
        
        assert_eq!(msg.target, Some([2u8; 32]));
    }
    
    #[test]
    fn test_ttl_decrement() {
        let mut msg = NetworkMessage::new([1u8; 32], MessageType::Ping, Vec::new());
        
        for i in (0..10).rev() {
            assert!(msg.decrement_ttl());
            assert_eq!(msg.ttl, i);
        }
        
        assert!(!msg.decrement_ttl());
    }
    
    #[test]
    fn test_handshake_data() {
        let handshake = HandshakeData::new("1.0.0".to_string(), [0u8; 32]);
        
        let bytes = handshake.to_bytes();
        let decoded = HandshakeData::from_bytes(&bytes).unwrap();
        
        assert_eq!(decoded.protocol_version, "1.0.0");
    }
}

