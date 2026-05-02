//! Node configuration

use serde::{Deserialize, Serialize};

/// Node configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Node settings
    pub node: NodeSettings,
    /// Network settings
    pub network: NetworkSettings,
    /// Consensus settings
    pub consensus: ConsensusSettings,
    /// Storage settings
    pub storage: StorageSettings,
    /// RPC settings
    pub rpc: RpcSettings,
    /// Metrics settings
    pub metrics: MetricsSettings,
    /// EVM backend settings (the local EVM execution layer — Reth in
    /// production, per `reth-blue-green-ipfs-architecture.mdc`).
    ///
    /// `#[serde(alias = "anvil")]` preserves backwards compatibility with
    /// existing operator deployments that still have an `[anvil]` section
    /// in their TOML config from the pre-2026-03-31 Anvil era.
    #[serde(default, alias = "anvil")]
    pub evm_backend: EvmBackendSettings,
    /// IoT Gateway settings
    #[serde(default)]
    pub iot_gateway: IoTGatewaySettings,
    /// AI Agent Framework settings
    #[serde(default)]
    pub ai_framework: AIFrameworkSettings,
}

/// EVM backend settings — configures the local EVM execution layer.
///
/// In production this is Reth v1.11.2 listening on `127.0.0.1:8595`
/// (per `reth-blue-green-ipfs-architecture.mdc`). Pre-2026-03-31 the
/// execution layer was Anvil; the migration was transparent at this layer
/// because both speak the same JSON-RPC dialect.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmBackendSettings {
    /// Enable the EVM backend
    pub enabled: bool,
    /// Local EVM execution layer's internal HTTP URL (never exposed publicly)
    pub url: String,
    /// Health check interval (seconds)
    pub health_interval_secs: u64,
    /// Max consecutive health check failures before marking unhealthy
    pub max_failures: u32,
}

impl Default for EvmBackendSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            // Production Reth listens on 8595 (was 8548 under Anvil).
            url: "http://127.0.0.1:8595".to_string(),
            health_interval_secs: 30,
            max_failures: 5,
        }
    }
}

/// Deprecated alias for [`EvmBackendSettings`]. Existing operator TOML
/// configs may still reference this type name; new code should use
/// `EvmBackendSettings`.
#[deprecated(
    since = "0.2.0",
    note = "Use `EvmBackendSettings`. Anvil was archived 2026-03-31."
)]
pub type AnvilSettings = EvmBackendSettings;

/// Node settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSettings {
    /// Node name
    pub name: String,
    /// Node mode
    pub mode: NodeMode,
    /// Chain ID
    pub chain_id: u64,
    /// External IP (for discovery)
    pub external_ip: Option<String>,
}

/// Node operation mode
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeMode {
    /// Full validator node
    Validator,
    /// Relay node (no validation)
    Relay,
    /// Seeder node (bootstrap)
    Seeder,
    /// Light client
    Light,
}

/// Network settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkSettings {
    /// P2P listen address
    pub listen_addr: String,
    /// Bootstrap nodes
    pub bootstrap_nodes: Vec<String>,
    /// Maximum peers
    pub max_peers: usize,
    /// Enable QUIC
    pub enable_quic: bool,
    /// Enable NAT traversal
    pub enable_nat: bool,
}

/// Consensus settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsensusSettings {
    /// Enable consensus participation
    pub enabled: bool,
    /// Block time target (ms)
    pub block_time_ms: u64,
    /// Minimum testimonies for finality
    pub min_testimonies: u32,
    /// AI agents enabled
    pub ai_agents_enabled: bool,
}

/// Storage settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageSettings {
    /// Database path
    pub db_path: String,
    /// Enable compression
    pub enable_compression: bool,
    /// Cache size (MB)
    pub cache_size_mb: usize,
    /// Pruning mode
    pub pruning: PruningMode,
}

/// Pruning mode
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PruningMode {
    /// Keep all data
    Archive,
    /// Keep recent data only
    Recent { blocks: u64 },
    /// Aggressive pruning
    Aggressive,
}

/// RPC settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcSettings {
    /// Enable RPC server
    pub enabled: bool,
    /// HTTP listen address
    pub http_addr: String,
    /// gRPC listen address
    pub grpc_addr: String,
    /// WebSocket listen address
    pub ws_addr: String,
    /// Enable TLS
    pub enable_tls: bool,
    /// TLS certificate path
    pub tls_cert: Option<String>,
    /// TLS key path
    pub tls_key: Option<String>,
    /// Enable mTLS (client certificates)
    pub enable_mtls: bool,
    /// Client CA path (for mTLS)
    pub client_ca: Option<String>,
    /// CORS allowed origins
    pub cors_origins: Vec<String>,
    /// Rate limit (requests/second)
    pub rate_limit: u32,
}

/// Metrics settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricsSettings {
    /// Enable metrics
    pub enabled: bool,
    /// Prometheus listen address
    pub prometheus_addr: String,
}

/// IoT Gateway settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IoTGatewaySettings {
    pub enabled: bool,
    pub mqtt_port: u16,
    pub coap_port: u16,
    pub max_devices: usize,
}

impl Default for IoTGatewaySettings {
    fn default() -> Self {
        Self {
            enabled: true,
            mqtt_port: 1883,
            coap_port: 5683,
            max_devices: 10_000,
        }
    }
}

/// AI Agent Framework settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AIFrameworkSettings {
    pub enabled: bool,
    pub builtin_maintenance_agent: bool,
    pub builtin_anomaly_agent: bool,
    pub max_agents: usize,
    pub scheduler_interval_secs: u64,
}

impl Default for AIFrameworkSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            builtin_maintenance_agent: true,
            builtin_anomaly_agent: true,
            max_agents: 100,
            scheduler_interval_secs: 60,
        }
    }
}

impl NodeConfig {
    /// Create config for a specific network
    pub fn for_network(network: &str) -> anyhow::Result<Self> {
        match network {
            "mainnet" => Ok(Self::mainnet()),
            "testnet" => Ok(Self::testnet()),
            _ => anyhow::bail!("Unknown network: {}", network),
        }
    }

    /// Mainnet configuration
    pub fn mainnet() -> Self {
        Self {
            node: NodeSettings {
                name: "rope-mainnet-node".to_string(),
                mode: NodeMode::Relay,
                chain_id: 271828,
                external_ip: None,
            },
            network: NetworkSettings {
                listen_addr: "0.0.0.0:9000".to_string(),
                bootstrap_nodes: vec![
                    // Primary bootstrap node on VPS
                    "/ip4/92.243.26.189/tcp/9000/p2p/12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM".to_string(),
                ],
                max_peers: 50,
                enable_quic: true,
                enable_nat: true,
            },
            consensus: ConsensusSettings {
                enabled: false,
                block_time_ms: 3000,
                min_testimonies: 5,
                ai_agents_enabled: true,
            },
            storage: StorageSettings {
                db_path: "~/.rope/mainnet/db".to_string(),
                enable_compression: true,
                cache_size_mb: 512,
                pruning: PruningMode::Archive,
            },
            rpc: RpcSettings {
                enabled: true,
                http_addr: "127.0.0.1:8545".to_string(),
                grpc_addr: "127.0.0.1:9001".to_string(),
                ws_addr: "127.0.0.1:8546".to_string(),
                enable_tls: false,
                tls_cert: None,
                tls_key: None,
                enable_mtls: false,
                client_ca: None,
                cors_origins: vec!["*".to_string()],
                rate_limit: 100,
            },
            metrics: MetricsSettings {
                enabled: true,
                prometheus_addr: "127.0.0.1:9090".to_string(),
            },
            evm_backend: EvmBackendSettings::default(),
            iot_gateway: IoTGatewaySettings::default(),
            ai_framework: AIFrameworkSettings::default(),
        }
    }

    /// Testnet configuration
    pub fn testnet() -> Self {
        let mut config = Self::mainnet();
        config.node.name = "rope-testnet-node".to_string();
        config.node.chain_id = 271829;
        config.network.bootstrap_nodes = vec![
            // Primary testnet bootstrap node on VPS
            "/ip4/92.243.26.189/tcp/9000/p2p/12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM"
                .to_string(),
        ];
        config.storage.db_path = "~/.rope/testnet/db".to_string();
        // Enable consensus for testnet validators
        config.consensus.enabled = true;
        config.consensus.min_testimonies = 1;
        config.consensus.block_time_ms = 4200; // ~4.2 seconds per anchor
        config
    }
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self::mainnet()
    }
}

impl Default for NodeMode {
    fn default() -> Self {
        Self::Relay
    }
}
