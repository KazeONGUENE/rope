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

pub mod discovery;
pub mod gossip;
pub mod message;
pub mod peer;
pub mod rdp;
pub mod rpc;
pub mod swarm;
pub mod transport;

// Re-exports
pub use discovery::{DhtConfig, DiscoveryService, PeerInfo};
pub use gossip::{GossipConfig, GossipMessage, GossipProtocol};
pub use message::{MessageType, NetworkMessage};
pub use peer::{PeerId, PeerManager, PeerState};
pub use rdp::{RdpConfig, RopeDistributionProtocol, Swarm as RdpSwarm};
pub use rpc::RpcConfig;
pub use swarm::{RopeSwarmRuntime, SwarmCommand, SwarmConfig, SwarmNetworkEvent, SwarmStats};
pub use transport::{TransportConfig, TransportLayer};
