//! # Datachain Rope Network Layer
//! 
//! P2P networking using libp2p with QUIC transport.
//! 
//! ## Channels
//! 
//! | Channel | Protocol | Security |
//! |---------|----------|----------|
//! | Validator Gossip | libp2p + QUIC | TLS 1.3 + Kyber |
//! | String Distribution | RDP over UDP | OES encryption |
//! | Client RPC | gRPC + HTTP/2 | mTLS + JWT |
//! | Bridge Relay | WebSocket | Threshold ECDSA |

pub mod transport {
    //! Transport layer using libp2p QUIC
    
    use std::net::SocketAddr;
    
    /// Network transport configuration
    #[derive(Clone, Debug)]
    pub struct TransportConfig {
        pub listen_addr: SocketAddr,
        pub enable_quic: bool,
        pub enable_tcp: bool,
    }
    
    impl Default for TransportConfig {
        fn default() -> Self {
            Self {
                listen_addr: "0.0.0.0:9000".parse().unwrap(),
                enable_quic: true,
                enable_tcp: true,
            }
        }
    }
}

pub mod discovery {
    //! Node discovery and DHT
    
    use std::collections::HashSet;
    
    /// Discovery service for finding peers
    pub struct DiscoveryService {
        known_peers: HashSet<String>,
        bootstrap_nodes: Vec<String>,
    }
    
    impl DiscoveryService {
        pub fn new(bootstrap_nodes: Vec<String>) -> Self {
            Self {
                known_peers: HashSet::new(),
                bootstrap_nodes,
            }
        }
        
        pub fn add_peer(&mut self, peer_id: String) {
            self.known_peers.insert(peer_id);
        }
        
        pub fn known_peers(&self) -> &HashSet<String> {
            &self.known_peers
        }
    }
}

pub mod rpc {
    //! gRPC API server
    
    /// RPC server configuration
    #[derive(Clone, Debug)]
    pub struct RpcConfig {
        pub enabled: bool,
        pub listen_addr: String,
        pub max_connections: usize,
    }
    
    impl Default for RpcConfig {
        fn default() -> Self {
            Self {
                enabled: true,
                listen_addr: "0.0.0.0:9001".to_string(),
                max_connections: 100,
            }
        }
    }
}

pub mod gossip {
    //! Gossip-about-gossip protocol
    
    use std::collections::VecDeque;
    
    /// Gossip message for virtual voting
    #[derive(Clone, Debug)]
    pub struct GossipMessage {
        pub sender_id: [u8; 32],
        pub sequence: u64,
        pub parent_hashes: Vec<[u8; 32]>,
        pub payload_hash: [u8; 32],
        pub timestamp: u64,
    }
    
    /// Gossip history for virtual voting reconstruction
    pub struct GossipHistory {
        messages: VecDeque<GossipMessage>,
        max_history: usize,
    }
    
    impl GossipHistory {
        pub fn new(max_history: usize) -> Self {
            Self {
                messages: VecDeque::new(),
                max_history,
            }
        }
        
        pub fn add_message(&mut self, msg: GossipMessage) {
            self.messages.push_back(msg);
            if self.messages.len() > self.max_history {
                self.messages.pop_front();
            }
        }
        
        pub fn messages(&self) -> &VecDeque<GossipMessage> {
            &self.messages
        }
    }
}

// Re-exports
pub use transport::TransportConfig;
pub use discovery::DiscoveryService;
pub use rpc::RpcConfig;
pub use gossip::{GossipMessage, GossipHistory};
