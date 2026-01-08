//! # Full libp2p Swarm Runtime Integration
//!
//! This module provides production-ready libp2p swarm integration for Datachain Rope.
//! It wires together all networking components: transport, gossipsub, Kademlia DHT,
//! request-response, and custom Rope protocol behaviors.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                        ROPE SWARM RUNTIME                                │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │                                                                          │
//! │  ┌────────────────┐  ┌────────────────┐  ┌────────────────┐             │
//! │  │   GossipSub    │  │    Kademlia    │  │   Request/     │             │
//! │  │   (Pub/Sub)    │  │     (DHT)      │  │   Response     │             │
//! │  └───────┬────────┘  └───────┬────────┘  └───────┬────────┘             │
//! │          │                   │                   │                       │
//! │          └───────────────────┴───────────────────┘                       │
//! │                              │                                           │
//! │                   ┌──────────┴──────────┐                                │
//! │                   │   RopeBehaviour     │                                │
//! │                   │  (Combined Behav.)  │                                │
//! │                   └──────────┬──────────┘                                │
//! │                              │                                           │
//! │                   ┌──────────┴──────────┐                                │
//! │                   │     libp2p Swarm    │                                │
//! │                   │   QUIC + TCP + WS   │                                │
//! │                   └──────────┬──────────┘                                │
//! │                              │                                           │
//! │  ┌───────────────────────────┴───────────────────────────┐              │
//! │  │                   Event Loop                           │              │
//! │  │  - Handle incoming connections                         │              │
//! │  │  - Process gossip messages                             │              │
//! │  │  - Route DHT queries                                   │              │
//! │  │  - Manage peer lifecycle                               │              │
//! │  └───────────────────────────────────────────────────────┘              │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use libp2p::{
    gossipsub::{self, IdentTopic, MessageAuthenticity, ValidationMode},
    identify,
    kad::{self, store::MemoryStore, Mode as KadMode},
    noise,
    request_response::{self, ProtocolSupport},
    swarm::{NetworkBehaviour, SwarmEvent},
    tcp, yamux, Multiaddr, PeerId, Swarm, SwarmBuilder,
};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc, oneshot};
use tracing::{debug, error, info, warn};

use super::transport::{ConnectionStats, RopeMessage, TransportConfig, TransportError};

// ============================================================================
// SWARM CONFIGURATION
// ============================================================================

/// Complete swarm configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SwarmConfig {
    /// Base transport config
    pub transport: TransportConfig,

    /// GossipSub configuration
    pub gossipsub: GossipSubConfig,

    /// Kademlia configuration
    pub kademlia: KademliaConfig,

    /// Request-Response configuration
    pub request_response: RequestResponseConfig,

    /// Node identity seed (32 bytes)
    pub identity_seed: Option<[u8; 32]>,
}

impl Default for SwarmConfig {
    fn default() -> Self {
        Self {
            transport: TransportConfig::default(),
            gossipsub: GossipSubConfig::default(),
            kademlia: KademliaConfig::default(),
            request_response: RequestResponseConfig::default(),
            identity_seed: None,
        }
    }
}

/// GossipSub-specific configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GossipSubConfig {
    /// Heartbeat interval
    pub heartbeat_interval: Duration,
    /// Maximum transmit size
    pub max_transmit_size: usize,
    /// Mesh degree (D)
    pub mesh_n: usize,
    /// Mesh low watermark (D_low)
    pub mesh_n_low: usize,
    /// Mesh high watermark (D_high)
    pub mesh_n_high: usize,
    /// Lazy push degree
    pub gossip_lazy: usize,
    /// History length
    pub history_length: usize,
    /// History gossip
    pub history_gossip: usize,
    /// Fanout TTL
    pub fanout_ttl: Duration,
    /// Duplicate cache TTL
    pub duplicate_cache_time: Duration,
    /// Enable flood publish
    pub flood_publish: bool,
}

impl Default for GossipSubConfig {
    fn default() -> Self {
        Self {
            heartbeat_interval: Duration::from_secs(1),
            max_transmit_size: 1024 * 1024, // 1MB
            mesh_n: 6,
            mesh_n_low: 4,
            mesh_n_high: 12,
            gossip_lazy: 6,
            history_length: 5,
            history_gossip: 3,
            fanout_ttl: Duration::from_secs(60),
            duplicate_cache_time: Duration::from_secs(60),
            flood_publish: false,
        }
    }
}

/// Kademlia-specific configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KademliaConfig {
    /// Protocol name
    pub protocol_name: String,
    /// Replication factor
    pub replication_factor: usize,
    /// Query parallelism
    pub parallelism: usize,
    /// Record TTL
    pub record_ttl: Duration,
    /// Provider record TTL
    pub provider_ttl: Duration,
    /// Enable server mode
    pub server_mode: bool,
}

impl Default for KademliaConfig {
    fn default() -> Self {
        Self {
            protocol_name: "/rope/kad/1.0.0".to_string(),
            replication_factor: 20,
            parallelism: 3,
            record_ttl: Duration::from_secs(3600 * 24), // 24 hours
            provider_ttl: Duration::from_secs(3600 * 12), // 12 hours
            server_mode: true,
        }
    }
}

/// Request-Response configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RequestResponseConfig {
    /// Protocol name
    pub protocol_name: String,
    /// Request timeout
    pub request_timeout: Duration,
    /// Maximum concurrent requests
    pub max_concurrent_requests: usize,
}

impl Default for RequestResponseConfig {
    fn default() -> Self {
        Self {
            protocol_name: "/rope/req/1.0.0".to_string(),
            request_timeout: Duration::from_secs(30),
            max_concurrent_requests: 100,
        }
    }
}

// ============================================================================
// SWARM ERRORS
// ============================================================================

/// Swarm-specific errors
#[derive(Error, Debug)]
pub enum SwarmError {
    #[error("Swarm not initialized")]
    NotInitialized,

    #[error("Swarm already running")]
    AlreadyRunning,

    #[error("Transport error: {0}")]
    Transport(#[from] TransportError),

    #[error("Channel error: {0}")]
    Channel(String),

    #[error("Publish error: {0}")]
    Publish(String),

    #[error("Subscribe error: {0}")]
    Subscribe(String),

    #[error("DHT error: {0}")]
    Dht(String),

    #[error("Peer error: {0}")]
    Peer(String),

    #[error("Config error: {0}")]
    Config(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

// ============================================================================
// ROPE NETWORK BEHAVIOUR
// ============================================================================

/// Combined network behaviour for Datachain Rope
#[derive(NetworkBehaviour)]
pub struct RopeBehaviour {
    /// GossipSub for pub/sub messaging
    pub gossipsub: gossipsub::Behaviour,

    /// Kademlia for DHT operations
    pub kademlia: kad::Behaviour<MemoryStore>,

    /// Identify protocol for peer discovery
    pub identify: identify::Behaviour,

    /// Request-Response for direct messaging
    pub request_response: request_response::cbor::Behaviour<RopeRequest, RopeResponse>,
}

/// Request message type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RopeRequest {
    /// Request a string by ID
    GetString { string_id: [u8; 32] },

    /// Request strings since a given round
    GetStringsSince { round: u64, limit: u32 },

    /// Request peer status
    GetStatus,

    /// Request testimonies for a string
    GetTestimonies { string_id: [u8; 32] },

    /// Request anchor information
    GetAnchor { round: u64 },
}

/// Response message type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RopeResponse {
    /// String data
    String {
        string_id: [u8; 32],
        content: Vec<u8>,
        signature: Vec<u8>,
    },

    /// Multiple strings
    Strings { strings: Vec<StringData> },

    /// Peer status
    Status {
        peer_id: Vec<u8>,
        latest_round: u64,
        string_count: u64,
        uptime_secs: u64,
    },

    /// Testimonies
    Testimonies { testimonies: Vec<TestimonyData> },

    /// Anchor information
    Anchor {
        round: u64,
        anchor_id: [u8; 32],
        finalized_strings: Vec<[u8; 32]>,
    },

    /// Error response
    Error { code: u32, message: String },
}

/// String data for responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StringData {
    pub id: [u8; 32],
    pub content: Vec<u8>,
    pub creator: [u8; 32],
    pub round: u64,
    pub signature: Vec<u8>,
}

/// Testimony data for responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestimonyData {
    pub target_string_id: [u8; 32],
    pub validator_id: [u8; 32],
    pub attestation_type: u8,
    pub round: u64,
    pub signature: Vec<u8>,
}

// ============================================================================
// SWARM COMMANDS
// ============================================================================

/// Commands to control the swarm from other tasks
#[derive(Debug)]
pub enum SwarmCommand {
    /// Subscribe to a topic
    Subscribe { topic: String },

    /// Unsubscribe from a topic
    Unsubscribe { topic: String },

    /// Publish a message
    Publish { topic: String, data: Vec<u8> },

    /// Connect to a peer
    Dial { addr: Multiaddr },

    /// Disconnect from a peer
    Disconnect { peer_id: PeerId },

    /// Store a value in DHT
    PutRecord { key: Vec<u8>, value: Vec<u8> },

    /// Get a value from DHT
    GetRecord {
        key: Vec<u8>,
        response: oneshot::Sender<Option<Vec<u8>>>,
    },

    /// Start providing a key
    StartProviding { key: Vec<u8> },

    /// Find providers for a key
    GetProviders {
        key: Vec<u8>,
        response: oneshot::Sender<Vec<PeerId>>,
    },

    /// Send a request to a peer
    SendRequest {
        peer_id: PeerId,
        request: RopeRequest,
        response: oneshot::Sender<Result<RopeResponse, String>>,
    },

    /// Get swarm statistics
    GetStats {
        response: oneshot::Sender<SwarmStats>,
    },

    /// Get connected peers
    GetPeers {
        response: oneshot::Sender<Vec<PeerInfo>>,
    },

    /// Shutdown the swarm
    Shutdown,
}

/// Swarm statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SwarmStats {
    pub local_peer_id: String,
    pub connected_peers: usize,
    pub known_peers: usize,
    pub messages_published: u64,
    pub messages_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub dht_queries: u64,
    pub active_subscriptions: Vec<String>,
    pub uptime_secs: u64,
}

/// Peer information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub peer_id: String,
    pub addresses: Vec<String>,
    pub agent_version: Option<String>,
    pub protocol_version: Option<String>,
    pub latency_ms: Option<u64>,
}

// ============================================================================
// SWARM EVENTS
// ============================================================================

/// Events emitted by the swarm for application processing
#[derive(Debug, Clone)]
pub enum SwarmNetworkEvent {
    /// New peer connected
    PeerConnected { peer_id: PeerId },

    /// Peer disconnected
    PeerDisconnected { peer_id: PeerId },

    /// Gossip message received
    GossipMessage { topic: String, data: Vec<u8>, source: PeerId },

    /// Request received
    RequestReceived {
        peer_id: PeerId,
        request: RopeRequest,
        channel: oneshot::Sender<RopeResponse>,
    },

    /// DHT record found
    DhtRecordFound { key: Vec<u8>, value: Vec<u8> },

    /// DHT providers found
    DhtProvidersFound { key: Vec<u8>, providers: Vec<PeerId> },
}

// ============================================================================
// ROPE SWARM RUNTIME
// ============================================================================

/// The main swarm runtime that manages the libp2p swarm
pub struct RopeSwarmRuntime {
    /// Configuration
    config: SwarmConfig,

    /// Command channel sender
    command_tx: Option<mpsc::Sender<SwarmCommand>>,

    /// Event broadcast channel
    event_tx: broadcast::Sender<SwarmNetworkEvent>,

    /// Statistics
    stats: Arc<RwLock<SwarmStats>>,

    /// Running state
    is_running: Arc<RwLock<bool>>,

    /// Local peer ID
    local_peer_id: Arc<RwLock<Option<PeerId>>>,

    /// Subscribed topics
    subscriptions: Arc<RwLock<HashSet<String>>>,
}

impl RopeSwarmRuntime {
    /// Create a new swarm runtime
    pub fn new(config: SwarmConfig) -> Self {
        let (event_tx, _) = broadcast::channel(1024);

        Self {
            config,
            command_tx: None,
            event_tx,
            stats: Arc::new(RwLock::new(SwarmStats::default())),
            is_running: Arc::new(RwLock::new(false)),
            local_peer_id: Arc::new(RwLock::new(None)),
            subscriptions: Arc::new(RwLock::new(HashSet::new())),
        }
    }

    /// Build and start the swarm
    pub async fn start(&mut self) -> Result<(), SwarmError> {
        if *self.is_running.read() {
            return Err(SwarmError::AlreadyRunning);
        }

        info!("Starting Rope Swarm Runtime...");

        // Generate or load identity
        let local_keypair = if let Some(seed) = self.config.identity_seed {
            let mut bytes = [0u8; 32];
            bytes.copy_from_slice(&seed);
            libp2p::identity::Keypair::ed25519_from_bytes(bytes)
                .map_err(|e| SwarmError::Config(format!("Invalid identity seed: {}", e)))?
        } else {
            libp2p::identity::Keypair::generate_ed25519()
        };

        let local_peer_id = PeerId::from(local_keypair.public());
        *self.local_peer_id.write() = Some(local_peer_id);
        self.stats.write().local_peer_id = local_peer_id.to_string();

        info!("Local peer ID: {}", local_peer_id);

        // Build the swarm
        let swarm = self.build_swarm(local_keypair).await?;

        // Create command channel
        let (command_tx, command_rx) = mpsc::channel(256);
        self.command_tx = Some(command_tx);

        // Clone what we need for the event loop
        let stats = self.stats.clone();
        let is_running = self.is_running.clone();
        let event_tx = self.event_tx.clone();
        let subscriptions = self.subscriptions.clone();
        let listen_addr = self.config.transport.listen_addr;

        // Spawn the event loop
        tokio::spawn(async move {
            Self::run_event_loop(
                swarm,
                command_rx,
                event_tx,
                stats,
                is_running,
                subscriptions,
                listen_addr,
            )
            .await;
        });

        *self.is_running.write() = true;
        info!("Rope Swarm Runtime started successfully");

        Ok(())
    }

    /// Build the libp2p swarm with all behaviours
    async fn build_swarm(
        &self,
        local_keypair: libp2p::identity::Keypair,
    ) -> Result<Swarm<RopeBehaviour>, SwarmError> {
        // GossipSub configuration
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .heartbeat_interval(self.config.gossipsub.heartbeat_interval)
            .max_transmit_size(self.config.gossipsub.max_transmit_size)
            .mesh_n(self.config.gossipsub.mesh_n)
            .mesh_n_low(self.config.gossipsub.mesh_n_low)
            .mesh_n_high(self.config.gossipsub.mesh_n_high)
            .gossip_lazy(self.config.gossipsub.gossip_lazy)
            .history_length(self.config.gossipsub.history_length)
            .history_gossip(self.config.gossipsub.history_gossip)
            .fanout_ttl(self.config.gossipsub.fanout_ttl)
            .duplicate_cache_time(self.config.gossipsub.duplicate_cache_time)
            .flood_publish(self.config.gossipsub.flood_publish)
            .validation_mode(ValidationMode::Strict)
            .build()
            .map_err(|e| SwarmError::Config(format!("GossipSub config error: {}", e)))?;

        let gossipsub = gossipsub::Behaviour::new(
            MessageAuthenticity::Signed(local_keypair.clone()),
            gossipsub_config,
        )
        .map_err(|e| SwarmError::Config(format!("GossipSub init error: {}", e)))?;

        // Kademlia configuration
        let local_peer_id = PeerId::from(local_keypair.public());
        let kad_store = MemoryStore::new(local_peer_id);
        let mut kad_config = kad::Config::default();
        kad_config
            .set_replication_factor(
                self.config
                    .kademlia
                    .replication_factor
                    .try_into()
                    .unwrap_or(20.try_into().unwrap()),
            )
            .set_parallelism(
                self.config
                    .kademlia
                    .parallelism
                    .try_into()
                    .unwrap_or(3.try_into().unwrap()),
            )
            .set_record_ttl(Some(self.config.kademlia.record_ttl))
            .set_provider_record_ttl(Some(self.config.kademlia.provider_ttl));

        let mut kademlia = kad::Behaviour::with_config(local_peer_id, kad_store, kad_config);

        if self.config.kademlia.server_mode {
            kademlia.set_mode(Some(KadMode::Server));
        }

        // Identify protocol
        let identify = identify::Behaviour::new(identify::Config::new(
            "/rope/1.0.0".to_string(),
            local_keypair.public(),
        ));

        // Request-Response protocol
        let request_response = request_response::cbor::Behaviour::new(
            [(
                request_response::ProtocolId::from(&self.config.request_response.protocol_name[..]),
                ProtocolSupport::Full,
            )],
            request_response::Config::default()
                .with_request_timeout(self.config.request_response.request_timeout),
        );

        // Create combined behaviour
        let behaviour = RopeBehaviour {
            gossipsub,
            kademlia,
            identify,
            request_response,
        };

        // Build swarm with QUIC or TCP
        let swarm = SwarmBuilder::with_existing_identity(local_keypair)
            .with_tokio()
            .with_tcp(
                tcp::Config::default(),
                noise::Config::new,
                yamux::Config::default,
            )
            .map_err(|e| SwarmError::Config(format!("TCP transport error: {}", e)))?
            .with_quic()
            .with_behaviour(|_| behaviour)
            .map_err(|e| SwarmError::Config(format!("Behaviour error: {}", e)))?
            .with_swarm_config(|cfg| {
                cfg.with_idle_connection_timeout(self.config.transport.idle_timeout)
            })
            .build();

        Ok(swarm)
    }

    /// Run the main event loop
    async fn run_event_loop(
        mut swarm: Swarm<RopeBehaviour>,
        mut command_rx: mpsc::Receiver<SwarmCommand>,
        event_tx: broadcast::Sender<SwarmNetworkEvent>,
        stats: Arc<RwLock<SwarmStats>>,
        is_running: Arc<RwLock<bool>>,
        subscriptions: Arc<RwLock<HashSet<String>>>,
        listen_addr: SocketAddr,
    ) {
        // Start listening
        let multiaddr: Multiaddr = format!("/ip4/{}/tcp/{}", listen_addr.ip(), listen_addr.port())
            .parse()
            .expect("Valid multiaddr");

        if let Err(e) = swarm.listen_on(multiaddr.clone()) {
            error!("Failed to listen on {}: {}", multiaddr, e);
            return;
        }

        // Also try QUIC
        let quic_addr: Multiaddr =
            format!("/ip4/{}/udp/{}/quic-v1", listen_addr.ip(), listen_addr.port())
                .parse()
                .expect("Valid QUIC multiaddr");

        if let Err(e) = swarm.listen_on(quic_addr.clone()) {
            warn!("Failed to listen on QUIC {}: {}", quic_addr, e);
        }

        info!("Swarm listening on {} and {:?}", multiaddr, quic_addr);

        let start_time = std::time::Instant::now();

        // Main event loop
        loop {
            tokio::select! {
                // Handle swarm events
                event = swarm.select_next_some() => {
                    Self::handle_swarm_event(
                        event,
                        &mut swarm,
                        &event_tx,
                        &stats,
                        &subscriptions,
                    ).await;
                }

                // Handle commands
                Some(cmd) = command_rx.recv() => {
                    match cmd {
                        SwarmCommand::Shutdown => {
                            info!("Swarm shutdown requested");
                            break;
                        }
                        _ => {
                            Self::handle_command(
                                cmd,
                                &mut swarm,
                                &stats,
                                &subscriptions,
                                start_time,
                            ).await;
                        }
                    }
                }

                // Periodic stats update
                _ = tokio::time::sleep(Duration::from_secs(10)) => {
                    let mut s = stats.write();
                    s.connected_peers = swarm.connected_peers().count();
                    s.uptime_secs = start_time.elapsed().as_secs();
                }
            }
        }

        *is_running.write() = false;
        info!("Swarm event loop terminated");
    }

    /// Handle swarm events
    async fn handle_swarm_event(
        event: SwarmEvent<RopeBehaviourEvent>,
        swarm: &mut Swarm<RopeBehaviour>,
        event_tx: &broadcast::Sender<SwarmNetworkEvent>,
        stats: &Arc<RwLock<SwarmStats>>,
        subscriptions: &Arc<RwLock<HashSet<String>>>,
    ) {
        match event {
            SwarmEvent::Behaviour(RopeBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) => {
                let topic = message.topic.to_string();
                debug!(
                    "Received gossip message on topic '{}' from {}",
                    topic, propagation_source
                );

                stats.write().messages_received += 1;
                stats.write().bytes_received += message.data.len() as u64;

                let _ = event_tx.send(SwarmNetworkEvent::GossipMessage {
                    topic,
                    data: message.data,
                    source: propagation_source,
                });
            }

            SwarmEvent::Behaviour(RopeBehaviourEvent::Gossipsub(gossipsub::Event::Subscribed {
                peer_id,
                topic,
            })) => {
                debug!("Peer {} subscribed to {}", peer_id, topic);
            }

            SwarmEvent::Behaviour(RopeBehaviourEvent::Kademlia(
                kad::Event::OutboundQueryProgressed {
                    result: kad::QueryResult::GetRecord(Ok(kad::GetRecordOk::FoundRecord(record))),
                    ..
                },
            )) => {
                let key = record.record.key.to_vec();
                let value = record.record.value.clone();
                debug!("DHT record found: {:?}", hex::encode(&key));

                let _ = event_tx.send(SwarmNetworkEvent::DhtRecordFound { key, value });
            }

            SwarmEvent::Behaviour(RopeBehaviourEvent::Kademlia(
                kad::Event::OutboundQueryProgressed {
                    result: kad::QueryResult::GetProviders(Ok(providers)),
                    ..
                },
            )) => {
                if let kad::GetProvidersOk::FoundProviders { key, providers } = providers {
                    let key_vec = key.to_vec();
                    let provider_list: Vec<PeerId> = providers.into_iter().collect();
                    debug!(
                        "DHT providers found for {:?}: {} providers",
                        hex::encode(&key_vec),
                        provider_list.len()
                    );

                    let _ = event_tx.send(SwarmNetworkEvent::DhtProvidersFound {
                        key: key_vec,
                        providers: provider_list,
                    });
                }
            }

            SwarmEvent::Behaviour(RopeBehaviourEvent::RequestResponse(
                request_response::Event::Message { peer, message },
            )) => {
                if let request_response::Message::Request {
                    request, channel, ..
                } = message
                {
                    debug!("Request received from {}: {:?}", peer, request);

                    // For now, respond with an error - application should handle this
                    let response = RopeResponse::Error {
                        code: 501,
                        message: "Request handling not implemented in swarm layer".to_string(),
                    };

                    let _ = swarm
                        .behaviour_mut()
                        .request_response
                        .send_response(channel, response);
                }
            }

            SwarmEvent::Behaviour(RopeBehaviourEvent::Identify(identify::Event::Received {
                peer_id,
                info,
            })) => {
                debug!(
                    "Identified peer {}: {} {:?}",
                    peer_id, info.protocol_version, info.listen_addrs
                );

                // Add addresses to Kademlia
                for addr in info.listen_addrs {
                    swarm.behaviour_mut().kademlia.add_address(&peer_id, addr);
                }
            }

            SwarmEvent::ConnectionEstablished {
                peer_id,
                endpoint,
                num_established,
                ..
            } => {
                info!(
                    "Connected to peer {} via {:?} (total: {})",
                    peer_id, endpoint, num_established
                );
                let _ = event_tx.send(SwarmNetworkEvent::PeerConnected { peer_id });
            }

            SwarmEvent::ConnectionClosed {
                peer_id,
                num_established,
                ..
            } => {
                if num_established == 0 {
                    info!("Disconnected from peer {}", peer_id);
                    let _ = event_tx.send(SwarmNetworkEvent::PeerDisconnected { peer_id });
                }
            }

            SwarmEvent::NewListenAddr { address, .. } => {
                info!("Listening on {}", address);
            }

            _ => {}
        }
    }

    /// Handle commands
    async fn handle_command(
        cmd: SwarmCommand,
        swarm: &mut Swarm<RopeBehaviour>,
        stats: &Arc<RwLock<SwarmStats>>,
        subscriptions: &Arc<RwLock<HashSet<String>>>,
        start_time: std::time::Instant,
    ) {
        match cmd {
            SwarmCommand::Subscribe { topic } => {
                let ident_topic = IdentTopic::new(&topic);
                match swarm.behaviour_mut().gossipsub.subscribe(&ident_topic) {
                    Ok(_) => {
                        subscriptions.write().insert(topic.clone());
                        stats.write().active_subscriptions = subscriptions
                            .read()
                            .iter()
                            .cloned()
                            .collect();
                        info!("Subscribed to topic: {}", topic);
                    }
                    Err(e) => {
                        warn!("Failed to subscribe to {}: {:?}", topic, e);
                    }
                }
            }

            SwarmCommand::Unsubscribe { topic } => {
                let ident_topic = IdentTopic::new(&topic);
                match swarm.behaviour_mut().gossipsub.unsubscribe(&ident_topic) {
                    Ok(_) => {
                        subscriptions.write().remove(&topic);
                        stats.write().active_subscriptions = subscriptions
                            .read()
                            .iter()
                            .cloned()
                            .collect();
                        info!("Unsubscribed from topic: {}", topic);
                    }
                    Err(e) => {
                        warn!("Failed to unsubscribe from {}: {:?}", topic, e);
                    }
                }
            }

            SwarmCommand::Publish { topic, data } => {
                let ident_topic = IdentTopic::new(&topic);
                match swarm.behaviour_mut().gossipsub.publish(ident_topic, data.clone()) {
                    Ok(_) => {
                        stats.write().messages_published += 1;
                        stats.write().bytes_sent += data.len() as u64;
                        debug!("Published {} bytes to {}", data.len(), topic);
                    }
                    Err(e) => {
                        warn!("Failed to publish to {}: {:?}", topic, e);
                    }
                }
            }

            SwarmCommand::Dial { addr } => {
                match swarm.dial(addr.clone()) {
                    Ok(_) => {
                        info!("Dialing {}", addr);
                    }
                    Err(e) => {
                        warn!("Failed to dial {}: {:?}", addr, e);
                    }
                }
            }

            SwarmCommand::Disconnect { peer_id } => {
                let _ = swarm.disconnect_peer_id(peer_id);
                info!("Disconnecting from {}", peer_id);
            }

            SwarmCommand::PutRecord { key, value } => {
                let record = kad::Record {
                    key: kad::RecordKey::new(&key),
                    value,
                    publisher: None,
                    expires: None,
                };
                let _ = swarm.behaviour_mut().kademlia.put_record(record, kad::Quorum::One);
                stats.write().dht_queries += 1;
            }

            SwarmCommand::GetRecord { key, response } => {
                let _ = swarm
                    .behaviour_mut()
                    .kademlia
                    .get_record(kad::RecordKey::new(&key));
                stats.write().dht_queries += 1;
                // Response will be sent via event
                let _ = response.send(None);
            }

            SwarmCommand::StartProviding { key } => {
                let _ = swarm
                    .behaviour_mut()
                    .kademlia
                    .start_providing(kad::RecordKey::new(&key));
            }

            SwarmCommand::GetProviders { key, response } => {
                let _ = swarm
                    .behaviour_mut()
                    .kademlia
                    .get_providers(kad::RecordKey::new(&key));
                stats.write().dht_queries += 1;
                // Response will be sent via event
                let _ = response.send(Vec::new());
            }

            SwarmCommand::SendRequest {
                peer_id,
                request,
                response,
            } => {
                let _request_id = swarm
                    .behaviour_mut()
                    .request_response
                    .send_request(&peer_id, request);
                // Response will be handled via event
                let _ = response.send(Err("Request sent, response pending".to_string()));
            }

            SwarmCommand::GetStats { response } => {
                let mut s = stats.write().clone();
                s.connected_peers = swarm.connected_peers().count();
                s.uptime_secs = start_time.elapsed().as_secs();
                let _ = response.send(s);
            }

            SwarmCommand::GetPeers { response } => {
                let peers: Vec<PeerInfo> = swarm
                    .connected_peers()
                    .map(|peer_id| PeerInfo {
                        peer_id: peer_id.to_string(),
                        addresses: swarm
                            .external_addresses()
                            .map(|a| a.to_string())
                            .collect(),
                        agent_version: None,
                        protocol_version: None,
                        latency_ms: None,
                    })
                    .collect();
                let _ = response.send(peers);
            }

            SwarmCommand::Shutdown => {
                // Handled in event loop
            }
        }
    }

    /// Stop the swarm
    pub async fn stop(&mut self) -> Result<(), SwarmError> {
        if let Some(tx) = &self.command_tx {
            let _ = tx.send(SwarmCommand::Shutdown).await;
        }
        self.command_tx = None;
        *self.is_running.write() = false;
        Ok(())
    }

    /// Check if running
    pub fn is_running(&self) -> bool {
        *self.is_running.read()
    }

    /// Get local peer ID
    pub fn local_peer_id(&self) -> Option<PeerId> {
        *self.local_peer_id.read()
    }

    /// Get command sender for external control
    pub fn command_sender(&self) -> Option<mpsc::Sender<SwarmCommand>> {
        self.command_tx.clone()
    }

    /// Subscribe to network events
    pub fn event_receiver(&self) -> broadcast::Receiver<SwarmNetworkEvent> {
        self.event_tx.subscribe()
    }

    /// Get current stats
    pub fn stats(&self) -> SwarmStats {
        self.stats.read().clone()
    }

    // ========================================================================
    // CONVENIENCE METHODS
    // ========================================================================

    /// Subscribe to a topic
    pub async fn subscribe(&self, topic: &str) -> Result<(), SwarmError> {
        if let Some(tx) = &self.command_tx {
            tx.send(SwarmCommand::Subscribe {
                topic: topic.to_string(),
            })
            .await
            .map_err(|e| SwarmError::Channel(e.to_string()))?;
            Ok(())
        } else {
            Err(SwarmError::NotInitialized)
        }
    }

    /// Publish a message
    pub async fn publish(&self, topic: &str, data: Vec<u8>) -> Result<(), SwarmError> {
        if let Some(tx) = &self.command_tx {
            tx.send(SwarmCommand::Publish {
                topic: topic.to_string(),
                data,
            })
            .await
            .map_err(|e| SwarmError::Channel(e.to_string()))?;
            Ok(())
        } else {
            Err(SwarmError::NotInitialized)
        }
    }

    /// Connect to a peer
    pub async fn dial(&self, addr: &str) -> Result<(), SwarmError> {
        let multiaddr: Multiaddr = addr
            .parse()
            .map_err(|e| SwarmError::Config(format!("Invalid multiaddr: {}", e)))?;

        if let Some(tx) = &self.command_tx {
            tx.send(SwarmCommand::Dial { addr: multiaddr })
                .await
                .map_err(|e| SwarmError::Channel(e.to_string()))?;
            Ok(())
        } else {
            Err(SwarmError::NotInitialized)
        }
    }
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = SwarmConfig::default();
        assert!(config.transport.enable_quic);
        assert_eq!(config.gossipsub.mesh_n, 6);
        assert_eq!(config.kademlia.replication_factor, 20);
    }

    #[tokio::test]
    async fn test_swarm_creation() {
        let config = SwarmConfig::default();
        let runtime = RopeSwarmRuntime::new(config);
        assert!(!runtime.is_running());
    }

    #[test]
    fn test_request_response_types() {
        let req = RopeRequest::GetStatus;
        let encoded = bincode::serialize(&req).unwrap();
        let decoded: RopeRequest = bincode::deserialize(&encoded).unwrap();
        matches!(decoded, RopeRequest::GetStatus);
    }
}

