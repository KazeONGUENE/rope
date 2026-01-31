//! Datachain Rope Node implementation
//!
//! Full node implementation with integrated libp2p swarm networking
//! and string production.

use crate::config::{NodeConfig, NodeMode};
use crate::rpc_server::RpcServer;
use crate::metrics::MetricsServer;
use crate::string_producer::{StringProducer, StringProducerConfig, ProductionEvent};
use crate::genesis;

use parking_lot::RwLock;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc};
use tokio::signal;
use rope_core::types::{NodeId, StringId};

// Import rope-network swarm runtime
use rope_network::{
    RopeSwarmRuntime, SwarmConfig, SwarmNetworkEvent, SwarmCommand,
    TransportConfig,
    swarm::{GossipSubConfig, KademliaConfig, RequestResponseConfig},
};

/// Node state
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NodeState {
    /// Node is starting up
    Starting,
    /// Node is syncing
    Syncing,
    /// Node is running normally
    Running,
    /// Node is shutting down
    Stopping,
    /// Node has stopped
    Stopped,
}

/// Datachain Rope Node
pub struct RopeNode {
    /// Configuration
    config: NodeConfig,
    /// Data directory
    data_dir: PathBuf,
    /// Node state
    state: Arc<RwLock<NodeState>>,
    /// Shutdown signal sender
    shutdown_tx: Option<mpsc::Sender<()>>,
    /// libp2p Swarm runtime
    swarm_runtime: Arc<RwLock<Option<RopeSwarmRuntime>>>,
    /// Network event receiver
    network_event_rx: Arc<RwLock<Option<broadcast::Receiver<SwarmNetworkEvent>>>>,
    /// Identity seed for deterministic peer ID
    identity_seed: Option<[u8; 32]>,
    /// Node ID
    node_id: Option<NodeId>,
    /// String producer shutdown channel
    producer_shutdown_tx: Option<mpsc::Sender<()>>,
    /// Current anchor/block number
    current_round: Arc<RwLock<u64>>,
}

impl RopeNode {
    /// Create a new node
    pub async fn new(config: NodeConfig, data_dir: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            data_dir,
            state: Arc::new(RwLock::new(NodeState::Starting)),
            shutdown_tx: None,
            swarm_runtime: Arc::new(RwLock::new(None)),
            network_event_rx: Arc::new(RwLock::new(None)),
            identity_seed: None,
            node_id: None,
            producer_shutdown_tx: None,
            current_round: Arc::new(RwLock::new(0)),
        })
    }

    /// Get current state
    pub fn state(&self) -> NodeState {
        self.state.read().clone()
    }
    
    /// Get current block/anchor number
    pub fn block_number(&self) -> u64 {
        *self.current_round.read()
    }

    /// Get swarm command sender for external control
    pub fn swarm_command_sender(&self) -> Option<mpsc::Sender<SwarmCommand>> {
        self.swarm_runtime.read().as_ref().and_then(|s| s.command_sender())
    }

    /// Run the node
    pub async fn run(&mut self) -> anyhow::Result<()> {
        tracing::info!("Starting Datachain Rope node...");

        // Set state to starting
        *self.state.write() = NodeState::Starting;

        // Initialize components
        self.init_storage().await?;
        let (identity_seed, node_id) = self.init_crypto().await?;
        self.identity_seed = Some(identity_seed);
        self.node_id = Some(node_id.clone());

        // Initialize and start libp2p network
        self.init_network(identity_seed).await?;

        // Initialize genesis if needed
        let genesis_string_id = self.init_genesis().await?;

        // Start string producer if validator
        let producer_handle = if self.config.consensus.enabled && 
            matches!(self.config.node.mode, NodeMode::Validator) {
            Some(self.start_string_producer(node_id, genesis_string_id).await?)
        } else {
            tracing::info!("String production disabled (non-validator mode)");
            None
        };

        // Start RPC server
        let rpc_handle = if self.config.rpc.enabled {
            let current_round = self.current_round.clone();
            let chain_id = self.config.node.chain_id;
            let rpc_server = RpcServer::new_with_state(
                &self.config.rpc, 
                chain_id,
                current_round,
            ).await?;
            Some(tokio::spawn(async move {
                if let Err(e) = rpc_server.run().await {
                    tracing::error!("RPC server error: {}", e);
                }
            }))
        } else {
            None
        };

        // Start metrics server
        let metrics_handle = if self.config.metrics.enabled {
            let metrics_server = MetricsServer::new(&self.config.metrics)?;
            Some(tokio::spawn(async move {
                if let Err(e) = metrics_server.run().await {
                    tracing::error!("Metrics server error: {}", e);
                }
            }))
        } else {
            None
        };

        // Start network event processing
        let network_handle = self.start_network_event_processor();

        // Set state to running
        *self.state.write() = NodeState::Running;

        self.print_startup_banner();

        // Wait for shutdown signal
        self.wait_for_shutdown().await;

        // Graceful shutdown
        *self.state.write() = NodeState::Stopping;
        tracing::info!("Shutting down...");

        // Stop string producer
        if let Some(tx) = self.producer_shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Stop swarm
        self.stop_network().await?;

        // Stop other components
        if let Some(handle) = rpc_handle {
            handle.abort();
        }
        if let Some(handle) = metrics_handle {
            handle.abort();
        }
        if let Some(handle) = network_handle {
            handle.abort();
        }
        if let Some(handle) = producer_handle {
            handle.abort();
        }

        *self.state.write() = NodeState::Stopped;
        tracing::info!("Node stopped");

        Ok(())
    }

    /// Initialize genesis
    async fn init_genesis(&self) -> anyhow::Result<StringId> {
        let genesis_path = self.data_dir.join("genesis.json");
        
        let genesis = if genesis_path.exists() {
            let content = std::fs::read_to_string(&genesis_path)?;
            serde_json::from_str(&content)?
        } else {
            // Generate genesis based on chain ID
            let gen = if self.config.node.chain_id == 271829 {
                genesis::generate_testnet_genesis()?
            } else {
                genesis::generate_genesis(1, self.config.node.chain_id)?
            };
            
            // Save genesis
            let content = serde_json::to_string_pretty(&gen)?;
            std::fs::write(&genesis_path, &content)?;
            tracing::info!("Genesis saved to {:?}", genesis_path);
            
            gen
        };
        
        tracing::info!("Genesis hash: {}", hex::encode(&genesis.genesis_hash[..8]));
        tracing::info!("Genesis string: {}", hex::encode(&genesis.genesis_string_id[..8]));
        
        Ok(StringId::new(genesis.genesis_string_id))
    }

    /// Start the string producer
    async fn start_string_producer(
        &mut self, 
        node_id: NodeId,
        genesis_string_id: StringId,
    ) -> anyhow::Result<tokio::task::JoinHandle<()>> {
        let config = StringProducerConfig {
            string_interval_ms: self.config.consensus.block_time_ms,
            min_testimonies: self.config.consensus.min_testimonies,
            max_pending_strings: 1000,
            enabled: true,
            is_validator: matches!(self.config.node.mode, NodeMode::Validator),
        };
        
        let mut producer = StringProducer::new(config, node_id);
        producer.set_genesis(genesis_string_id);
        
        // Get event receiver for updating state
        let mut event_rx = producer.subscribe();
        let current_round = self.current_round.clone();
        let swarm = self.swarm_runtime.clone();
        
        // Spawn event handler
        tokio::spawn(async move {
            while let Ok(event) = event_rx.recv().await {
                match event {
                    ProductionEvent::AnchorFinalized { anchor_id, round, strings_included: _ } => {
                        *current_round.write() = round;
                        
                        // Broadcast anchor to network
                        // Clone swarm reference to avoid holding lock across await
                        let publish_result = {
                            let swarm_guard = swarm.read();
                            if let Some(sw) = swarm_guard.as_ref() {
                                let msg = format!("anchor:{}:{}", round, hex::encode(&anchor_id.as_bytes()[..16]));
                                Some((sw.command_sender(), msg))
                            } else {
                                None
                            }
                        };
                        
                        if let Some((Some(cmd_tx), msg)) = publish_result {
                            // Use command channel instead of direct publish
                            let _ = cmd_tx.send(rope_network::SwarmCommand::Publish {
                                topic: "/rope/anchors/1.0.0".to_string(),
                                data: msg.into_bytes(),
                            }).await;
                        }
                    }
                    ProductionEvent::ProductionError { round, error } => {
                        tracing::warn!("Production error at round {}: {}", round, error);
                    }
                    _ => {}
                }
            }
        });
        
        // Create shutdown channel
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);
        self.producer_shutdown_tx = Some(shutdown_tx);
        
        // Start producer
        let handle = tokio::spawn(async move {
            producer.run(shutdown_rx).await;
        });
        
        tracing::info!("String producer started (interval: {}ms)", 
            self.config.consensus.block_time_ms);
        
        Ok(handle)
    }

    /// Print startup banner with node information
    fn print_startup_banner(&self) {
        tracing::info!("╔══════════════════════════════════════════════════════════════╗");
        tracing::info!("║              DATACHAIN ROPE NODE IS RUNNING                  ║");
        tracing::info!("╚══════════════════════════════════════════════════════════════╝");
        tracing::info!("");
        tracing::info!("Chain ID: {}", self.config.node.chain_id);
        tracing::info!("Mode: {:?}", self.config.node.mode);

        // Print peer ID if swarm is running
        if let Some(swarm) = self.swarm_runtime.read().as_ref() {
            if let Some(peer_id) = swarm.local_peer_id() {
                tracing::info!("Peer ID: {}", peer_id);
            }
        }

        tracing::info!("P2P Listen: {}", self.config.network.listen_addr);
        tracing::info!("Bootstrap nodes: {}", self.config.network.bootstrap_nodes.len());

        if self.config.rpc.enabled {
            tracing::info!("HTTP RPC: http://{}", self.config.rpc.http_addr);
            tracing::info!("gRPC: {}", self.config.rpc.grpc_addr);
            tracing::info!("WebSocket: ws://{}", self.config.rpc.ws_addr);
        }

        if self.config.metrics.enabled {
            tracing::info!("Metrics: http://{}/metrics", self.config.metrics.prometheus_addr);
        }
        
        if self.config.consensus.enabled && matches!(self.config.node.mode, NodeMode::Validator) {
            tracing::info!("String Production: ENABLED ({}ms interval)", 
                self.config.consensus.block_time_ms);
        }

        tracing::info!("");
        tracing::info!("Press Ctrl+C to stop the node");
    }

    /// Initialize storage
    async fn init_storage(&self) -> anyhow::Result<()> {
        tracing::info!("Initializing storage...");

        let db_path = self.data_dir.join("db");
        std::fs::create_dir_all(&db_path)?;

        tracing::info!("Storage initialized at {:?}", db_path);
        Ok(())
    }

    /// Initialize cryptography and return identity seed and node ID
    async fn init_crypto(&self) -> anyhow::Result<([u8; 32], NodeId)> {
        tracing::info!("Initializing cryptography (OES with post-quantum support)...");

        let keys_path = self.data_dir.join("keys");
        std::fs::create_dir_all(&keys_path)?;

        // Load or generate keys
        let node_key_path = keys_path.join("node.key");
        let identity_seed: [u8; 32];
        let node_id: NodeId;

        if !node_key_path.exists() {
            tracing::info!("Generating node keys with hybrid post-quantum cryptography...");
            let keypair = rope_crypto::keys::KeyPair::generate_hybrid()?;

            // Save keys
            let private_key_bytes = keypair.private_key_bytes();
            std::fs::write(&node_key_path, &private_key_bytes)?;
            std::fs::write(keys_path.join("node.pub"), keypair.public_key_bytes())?;
            std::fs::write(keys_path.join("node.id"), hex::encode(keypair.node_id()))?;

            // Use first 32 bytes of private key as identity seed for libp2p
            identity_seed = private_key_bytes[..32].try_into()
                .map_err(|_| anyhow::anyhow!("Failed to extract identity seed from keypair"))?;
            
            node_id = NodeId::new(keypair.node_id());

            tracing::info!("Node ID: {}", hex::encode(keypair.node_id()));
            tracing::info!("Keys saved to {:?}", keys_path);
        } else {
            let private_key_bytes = std::fs::read(&node_key_path)?;
            let id_bytes = std::fs::read(keys_path.join("node.id"))?;
            let id_hex = String::from_utf8_lossy(&id_bytes);

            // Extract identity seed from saved private key
            identity_seed = private_key_bytes[..32].try_into()
                .map_err(|_| anyhow::anyhow!("Invalid private key format"))?;
            
            // Parse node ID from hex
            let id_bytes = hex::decode(id_hex.trim())?;
            let mut id_arr = [0u8; 32];
            id_arr.copy_from_slice(&id_bytes[..32]);
            node_id = NodeId::new(id_arr);

            tracing::info!("Node ID: {}", id_hex.trim());
        }

        tracing::info!("Cryptography initialized (Ed25519 + Dilithium3 + Kyber768)");
        Ok((identity_seed, node_id))
    }

    /// Initialize networking with libp2p swarm
    async fn init_network(&mut self, identity_seed: [u8; 32]) -> anyhow::Result<()> {
        tracing::info!("Initializing P2P network with libp2p swarm...");

        // Parse listen address from config
        let listen_addr: SocketAddr = self.config.network.listen_addr
            .parse()
            .unwrap_or_else(|_| "0.0.0.0:9000".parse().unwrap());

        // Build swarm configuration from node config
        let swarm_config = SwarmConfig {
            transport: TransportConfig {
                listen_addr,
                enable_quic: self.config.network.enable_quic,
                enable_tcp: true,
                enable_websocket: false,
                connection_timeout: Duration::from_secs(30),
                idle_timeout: Duration::from_secs(300),
                max_connections: self.config.network.max_peers,
                enable_pq_crypto: true,
                bootstrap_peers: self.config.network.bootstrap_nodes.clone(),
                enable_relay: self.config.network.enable_nat,
                gossip_heartbeat: Duration::from_secs(1),
                kad_replication: 20,
            },
            gossipsub: GossipSubConfig {
                heartbeat_interval: Duration::from_secs(1),
                max_transmit_size: 1024 * 1024, // 1MB max message
                mesh_n: 6,
                mesh_n_low: 4,
                mesh_n_high: 12,
                gossip_lazy: 6,
                history_length: 5,
                history_gossip: 3,
                fanout_ttl: Duration::from_secs(60),
                duplicate_cache_time: Duration::from_secs(60),
                flood_publish: false,
            },
            kademlia: KademliaConfig {
                protocol_name: "/rope/kad/1.0.0".to_string(),
                replication_factor: 20,
                parallelism: 3,
                record_ttl: Duration::from_secs(3600 * 24),
                provider_ttl: Duration::from_secs(3600 * 12),
                server_mode: matches!(self.config.node.mode, NodeMode::Validator),
            },
            request_response: RequestResponseConfig {
                protocol_name: "/rope/req/1.0.0".to_string(),
                request_timeout: Duration::from_secs(30),
                max_concurrent_requests: 100,
            },
            identity_seed: Some(identity_seed),
        };

        // Create and start swarm runtime
        let mut swarm_runtime = RopeSwarmRuntime::new(swarm_config);

        swarm_runtime.start().await
            .map_err(|e| anyhow::anyhow!("Failed to start swarm: {}", e))?;

        // Get event receiver before moving swarm_runtime
        let event_rx = swarm_runtime.event_receiver();

        // Log peer ID
        if let Some(peer_id) = swarm_runtime.local_peer_id() {
            tracing::info!("Local Peer ID: {}", peer_id);
        }

        // Subscribe to core topics
        swarm_runtime.subscribe("/rope/strings/1.0.0").await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to strings topic: {}", e))?;
        swarm_runtime.subscribe("/rope/gossip/1.0.0").await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to gossip topic: {}", e))?;
        swarm_runtime.subscribe("/rope/testimonies/1.0.0").await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to testimonies topic: {}", e))?;
        swarm_runtime.subscribe("/rope/anchors/1.0.0").await
            .map_err(|e| anyhow::anyhow!("Failed to subscribe to anchors topic: {}", e))?;

        // Connect to bootstrap nodes
        for bootstrap in &self.config.network.bootstrap_nodes {
            match swarm_runtime.dial(bootstrap).await {
                Ok(_) => tracing::info!("Dialing bootstrap node: {}", bootstrap),
                Err(e) => tracing::warn!("Failed to dial bootstrap {}: {}", bootstrap, e),
            }
        }

        // Store references
        *self.swarm_runtime.write() = Some(swarm_runtime);
        *self.network_event_rx.write() = Some(event_rx);

        tracing::info!("P2P network initialized with QUIC + TCP transport");
        tracing::info!("Subscribed to core protocol topics");
        tracing::info!("Bootstrap nodes: {}", self.config.network.bootstrap_nodes.len());

        Ok(())
    }

    /// Start network event processor
    fn start_network_event_processor(&self) -> Option<tokio::task::JoinHandle<()>> {
        let event_rx = self.network_event_rx.write().take()?;
        let state = self.state.clone();
        let current_round = self.current_round.clone();

        Some(tokio::spawn(async move {
            Self::process_network_events(event_rx, state, current_round).await;
        }))
    }

    /// Process network events from the swarm
    async fn process_network_events(
        mut event_rx: broadcast::Receiver<SwarmNetworkEvent>,
        state: Arc<RwLock<NodeState>>,
        current_round: Arc<RwLock<u64>>,
    ) {
        loop {
            // Check if we should stop
            if *state.read() == NodeState::Stopping || *state.read() == NodeState::Stopped {
                break;
            }

            match event_rx.recv().await {
                Ok(event) => {
                    match event {
                        SwarmNetworkEvent::PeerConnected { peer_id } => {
                            tracing::info!("Peer connected: {}", peer_id);
                        }
                        SwarmNetworkEvent::PeerDisconnected { peer_id } => {
                            tracing::info!("Peer disconnected: {}", peer_id);
                        }
                        SwarmNetworkEvent::GossipMessage { topic, data, source } => {
                            tracing::debug!(
                                "Gossip message on '{}' from {}: {} bytes",
                                topic, source, data.len()
                            );
                            // Process message based on topic
                            Self::handle_gossip_message(&topic, &data, &source, &current_round).await;
                        }
                        SwarmNetworkEvent::DhtRecordFound { key, value } => {
                            tracing::debug!(
                                "DHT record found: {} = {} bytes",
                                hex::encode(&key), value.len()
                            );
                        }
                        SwarmNetworkEvent::DhtProvidersFound { key, providers } => {
                            tracing::debug!(
                                "DHT providers for {}: {} providers",
                                hex::encode(&key), providers.len()
                            );
                        }
                        _ => {}
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::warn!("Network event processor lagged by {} events", n);
                }
                Err(broadcast::error::RecvError::Closed) => {
                    tracing::info!("Network event channel closed");
                    break;
                }
            }
        }
    }

    /// Handle incoming gossip messages
    async fn handle_gossip_message(
        topic: &str, 
        data: &[u8], 
        source: &libp2p::PeerId,
        current_round: &Arc<RwLock<u64>>,
    ) {
        match topic {
            "/rope/strings/1.0.0" => {
                tracing::trace!("Received string announcement from {}", source);
            }
            "/rope/gossip/1.0.0" => {
                tracing::trace!("Received gossip event from {}", source);
            }
            "/rope/testimonies/1.0.0" => {
                tracing::trace!("Received testimony from {}", source);
            }
            "/rope/anchors/1.0.0" => {
                // Parse anchor message
                if let Ok(msg) = String::from_utf8(data.to_vec()) {
                    if msg.starts_with("anchor:") {
                        let parts: Vec<&str> = msg.split(':').collect();
                        if parts.len() >= 2 {
                            if let Ok(round) = parts[1].parse::<u64>() {
                                let local_round = *current_round.read();
                                if round > local_round {
                                    tracing::info!(
                                        "Received anchor #{} from {} (local: #{})",
                                        round, source, local_round
                                    );
                                    // In a full implementation, we'd sync here
                                }
                            }
                        }
                    }
                }
            }
            _ => {
                tracing::trace!("Received message on unknown topic: {}", topic);
            }
        }
    }

    /// Stop the network
    async fn stop_network(&mut self) -> anyhow::Result<()> {
        if let Some(mut swarm) = self.swarm_runtime.write().take() {
            swarm.stop().await
                .map_err(|e| anyhow::anyhow!("Failed to stop swarm: {}", e))?;
            tracing::info!("P2P network stopped");
        }
        Ok(())
    }

    /// Wait for shutdown signal
    async fn wait_for_shutdown(&self) {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl+C handler");
        };

        #[cfg(unix)]
        let terminate = async {
            signal::unix::signal(signal::unix::SignalKind::terminate())
                .expect("Failed to install signal handler")
                .recv()
                .await;
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    }

    /// Publish a message to the network
    pub async fn publish(&self, topic: &str, data: Vec<u8>) -> anyhow::Result<()> {
        if let Some(swarm) = self.swarm_runtime.read().as_ref() {
            swarm.publish(topic, data).await
                .map_err(|e| anyhow::anyhow!("Publish failed: {}", e))?;
        }
        Ok(())
    }

    /// Get network statistics
    pub fn network_stats(&self) -> Option<rope_network::SwarmStats> {
        self.swarm_runtime.read().as_ref().map(|s| s.stats())
    }
}

impl Drop for RopeNode {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.blocking_send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NodeConfig;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_node_creation() {
        let config = NodeConfig::testnet();
        let temp_dir = TempDir::new().unwrap();

        let node = RopeNode::new(config, temp_dir.path().to_path_buf()).await;
        assert!(node.is_ok());

        let node = node.unwrap();
        assert_eq!(node.state(), NodeState::Starting);
    }
}
