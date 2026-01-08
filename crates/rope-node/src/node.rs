//! Datachain Rope Node implementation

use crate::config::{NodeConfig, NodeMode};
use crate::rpc_server::RpcServer;
use crate::metrics::MetricsServer;

use parking_lot::RwLock;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::signal;

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
}

impl RopeNode {
    /// Create a new node
    pub async fn new(config: NodeConfig, data_dir: PathBuf) -> anyhow::Result<Self> {
        Ok(Self {
            config,
            data_dir,
            state: Arc::new(RwLock::new(NodeState::Starting)),
            shutdown_tx: None,
        })
    }
    
    /// Get current state
    pub fn state(&self) -> NodeState {
        self.state.read().clone()
    }
    
    /// Run the node
    pub async fn run(&self) -> anyhow::Result<()> {
        tracing::info!("Starting Datachain Rope node...");
        
        // Set state to starting
        *self.state.write() = NodeState::Starting;
        
        // Initialize components
        self.init_storage().await?;
        self.init_crypto().await?;
        self.init_network().await?;
        
        if self.config.consensus.enabled {
            self.init_consensus().await?;
        }
        
        // Start RPC server
        let rpc_handle = if self.config.rpc.enabled {
            let rpc_server = RpcServer::new(&self.config.rpc).await?;
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
        
        // Set state to running
        *self.state.write() = NodeState::Running;
        
        tracing::info!("╔══════════════════════════════════════════════════════════════╗");
        tracing::info!("║                    NODE IS RUNNING                           ║");
        tracing::info!("╚══════════════════════════════════════════════════════════════╝");
        tracing::info!("");
        tracing::info!("Chain ID: {}", self.config.node.chain_id);
        tracing::info!("Mode: {:?}", self.config.node.mode);
        
        if self.config.rpc.enabled {
            tracing::info!("HTTP RPC: http://{}", self.config.rpc.http_addr);
            tracing::info!("gRPC: {}", self.config.rpc.grpc_addr);
            tracing::info!("WebSocket: ws://{}", self.config.rpc.ws_addr);
        }
        
        if self.config.metrics.enabled {
            tracing::info!("Metrics: http://{}/metrics", self.config.metrics.prometheus_addr);
        }
        
        tracing::info!("");
        tracing::info!("Press Ctrl+C to stop the node");
        
        // Wait for shutdown signal
        self.wait_for_shutdown().await;
        
        // Graceful shutdown
        *self.state.write() = NodeState::Stopping;
        tracing::info!("Shutting down...");
        
        // Stop components
        if let Some(handle) = rpc_handle {
            handle.abort();
        }
        if let Some(handle) = metrics_handle {
            handle.abort();
        }
        
        *self.state.write() = NodeState::Stopped;
        tracing::info!("Node stopped");
        
        Ok(())
    }
    
    /// Initialize storage
    async fn init_storage(&self) -> anyhow::Result<()> {
        tracing::info!("Initializing storage...");
        
        let db_path = self.data_dir.join("db");
        std::fs::create_dir_all(&db_path)?;
        
        tracing::info!("Storage initialized at {:?}", db_path);
        Ok(())
    }
    
    /// Initialize cryptography
    async fn init_crypto(&self) -> anyhow::Result<()> {
        tracing::info!("Initializing cryptography (OES)...");
        
        let keys_path = self.data_dir.join("keys");
        std::fs::create_dir_all(&keys_path)?;
        
        // Load or generate keys
        let node_key_path = keys_path.join("node.key");
        if !node_key_path.exists() {
            tracing::info!("Generating node keys...");
            let keypair = rope_crypto::keys::KeyPair::generate_hybrid()?;
            std::fs::write(&node_key_path, keypair.private_key_bytes())?;
            std::fs::write(keys_path.join("node.pub"), keypair.public_key_bytes())?;
            std::fs::write(keys_path.join("node.id"), hex::encode(keypair.node_id()))?;
            tracing::info!("Node ID: {}", hex::encode(keypair.node_id()));
        } else {
            let id_bytes = std::fs::read(keys_path.join("node.id"))?;
            tracing::info!("Node ID: {}", String::from_utf8_lossy(&id_bytes));
        }
        
        tracing::info!("Cryptography initialized");
        Ok(())
    }
    
    /// Initialize networking
    async fn init_network(&self) -> anyhow::Result<()> {
        tracing::info!("Initializing P2P network...");
        
        tracing::info!("Listen: {}", self.config.network.listen_addr);
        tracing::info!("Bootstrap nodes: {}", self.config.network.bootstrap_nodes.len());
        tracing::info!("Max peers: {}", self.config.network.max_peers);
        
        // TODO: Initialize libp2p swarm
        
        tracing::info!("Network initialized");
        Ok(())
    }
    
    /// Initialize consensus
    async fn init_consensus(&self) -> anyhow::Result<()> {
        tracing::info!("Initializing Testimony consensus...");
        
        tracing::info!("Block time: {}ms", self.config.consensus.block_time_ms);
        tracing::info!("Min testimonies: {}", self.config.consensus.min_testimonies);
        
        if self.config.consensus.ai_agents_enabled {
            tracing::info!("AI Testimony Agents: ENABLED");
        }
        
        tracing::info!("Consensus initialized");
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
}

impl Drop for RopeNode {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.blocking_send(());
        }
    }
}
