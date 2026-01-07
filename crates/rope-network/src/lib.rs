//! # Datachain Rope Network Layer
//! 
//! P2P networking using libp2p with QUIC transport.
//! 
//! ## Architecture
//! 
//! ```text
//! ┌─────────────────────────────────────────────────────────────────┐
//! │                     ROPE NETWORK LAYER                           │
//! ├─────────────────────────────────────────────────────────────────┤
//! │                                                                  │
//! │  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐          │
//! │  │   libp2p     │  │    Gossip    │  │     DHT      │          │
//! │  │  Transport   │  │   Protocol   │  │  Discovery   │          │
//! │  │  QUIC/TCP    │  │              │  │              │          │
//! │  └──────────────┘  └──────────────┘  └──────────────┘          │
//! │         │                 │                 │                   │
//! │         └─────────────────┴─────────────────┘                   │
//! │                          │                                      │
//! │                   ┌──────┴──────┐                               │
//! │                   │  RDP Layer  │                               │
//! │                   │ (BitTorrent │                               │
//! │                   │  inspired)  │                               │
//! │                   └─────────────┘                               │
//! └─────────────────────────────────────────────────────────────────┘
//! ```
//! 
//! ## Channels
//! 
//! | Channel | Protocol | Security |
//! |---------|----------|----------|
//! | Validator Gossip | libp2p + QUIC | TLS 1.3 + Kyber |
//! | String Distribution | RDP over UDP | OES encryption |
//! | Client RPC | gRPC + HTTP/2 | mTLS + JWT |
//! | Bridge Relay | WebSocket | Threshold ECDSA |

pub mod transport;
pub mod gossip;
pub mod discovery;
pub mod rdp;
pub mod peer;
pub mod message;
pub mod rpc;

// Re-exports
pub use transport::{TransportConfig, TransportLayer};
pub use gossip::{GossipProtocol, GossipMessage, GossipConfig};
pub use discovery::{DiscoveryService, DhtConfig, PeerInfo};
pub use rdp::{RopeDistributionProtocol, RdpConfig, Swarm as RdpSwarm};
pub use peer::{PeerId, PeerManager, PeerState};
pub use message::{NetworkMessage, MessageType};
pub use rpc::RpcConfig;
