//! Node configuration types

use serde::{Deserialize, Serialize};
use std::time::Duration;
use rope_core::types::GeoZone;

/// Complete node configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    /// Node operation settings
    #[serde(default)]
    pub node: NodeSettings,
    
    /// String lattice configuration
    #[serde(default)]
    pub lattice: LatticeConfig,
    
    /// OES configuration
    #[serde(default)]
    pub oes: OESConfig,
    
    /// Consensus parameters
    #[serde(default)]
    pub consensus: ConsensusConfig,
    
    /// Network settings
    #[serde(default)]
    pub network: NetworkConfig,
    
    /// Distribution protocol settings
    #[serde(default)]
    pub distribution: DistributionConfig,
    
    /// Rate limiting
    #[serde(default)]
    pub rate_limits: RateLimits,
    
    /// RPC API settings
    #[serde(default)]
    pub rpc: RPCConfig,
    
    /// Storage settings
    #[serde(default)]
    pub storage: StorageConfig,
    
    /// Logging configuration
    #[serde(default)]
    pub logging: LoggingConfig,
    
    /// Metrics configuration
    #[serde(default)]
    pub metrics: MetricsConfig,
    
    /// Validator-specific settings
    #[serde(default)]
    pub validator: ValidatorConfig,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            node: NodeSettings::default(),
            lattice: LatticeConfig::default(),
            oes: OESConfig::default(),
            consensus: ConsensusConfig::default(),
            network: NetworkConfig::default(),
            distribution: DistributionConfig::default(),
            rate_limits: RateLimits::default(),
            rpc: RPCConfig::default(),
            storage: StorageConfig::default(),
            logging: LoggingConfig::default(),
            metrics: MetricsConfig::default(),
            validator: ValidatorConfig::default(),
        }
    }
}

/// Node operation mode
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NodeMode {
    Validator,
    Relay,
    Seeder,
}

impl Default for NodeMode {
    fn default() -> Self {
        Self::Relay
    }
}

/// Basic node settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeSettings {
    /// Operation mode
    #[serde(default)]
    pub mode: NodeMode,
    
    /// Chain identifier
    #[serde(default = "default_chain_id")]
    pub chain_id: String,
    
    /// Node name
    #[serde(default = "default_node_name")]
    pub name: String,
    
    /// Data directory
    #[serde(default = "default_data_dir")]
    pub data_dir: String,
}

fn default_chain_id() -> String {
    "datachain-testnet-1".to_string()
}

fn default_node_name() -> String {
    "rope-node".to_string()
}

fn default_data_dir() -> String {
    "./data".to_string()
}

impl Default for NodeSettings {
    fn default() -> Self {
        Self {
            mode: NodeMode::default(),
            chain_id: default_chain_id(),
            name: default_node_name(),
            data_dir: default_data_dir(),
        }
    }
}

/// Lattice configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LatticeConfig {
    /// Replication factor (3-10)
    #[serde(default = "default_replication_factor")]
    pub replication_factor: u32,
    
    /// Enable erasure protocol
    #[serde(default = "default_true")]
    pub erasure_enabled: bool,
    
    /// Enable regeneration protocol
    #[serde(default = "default_true")]
    pub regeneration_enabled: bool,
    
    /// Maximum string size in bytes
    #[serde(default = "default_max_string_size")]
    pub max_string_size: usize,
}

fn default_replication_factor() -> u32 {
    5
}

fn default_true() -> bool {
    true
}

fn default_max_string_size() -> usize {
    10 * 1024 * 1024 // 10 MB
}

impl Default for LatticeConfig {
    fn default() -> Self {
        Self {
            replication_factor: default_replication_factor(),
            erasure_enabled: true,
            regeneration_enabled: true,
            max_string_size: default_max_string_size(),
        }
    }
}

/// OES configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OESConfig {
    /// Evolution interval in anchors
    #[serde(default = "default_evolution_interval")]
    pub evolution_interval: u64,
    
    /// Genome dimension in bytes
    #[serde(default = "default_genome_dimension")]
    pub genome_dimension: usize,
    
    /// Mutation rate (0.0-1.0)
    #[serde(default = "default_mutation_rate")]
    pub mutation_rate: f64,
    
    /// Valid generations window
    #[serde(default = "default_generation_window")]
    pub generation_window: u64,
}

fn default_evolution_interval() -> u64 {
    100
}

fn default_genome_dimension() -> usize {
    992
}

fn default_mutation_rate() -> f64 {
    0.1
}

fn default_generation_window() -> u64 {
    10
}

impl Default for OESConfig {
    fn default() -> Self {
        Self {
            evolution_interval: default_evolution_interval(),
            genome_dimension: default_genome_dimension(),
            mutation_rate: default_mutation_rate(),
            generation_window: default_generation_window(),
        }
    }
}

/// Consensus configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConsensusConfig {
    /// Anchor interval in seconds
    #[serde(default = "default_anchor_interval")]
    pub anchor_interval: u64,
    
    /// Anchors required for finality
    #[serde(default = "default_finality_anchors")]
    pub finality_anchors: u32,
    
    /// Maximum strings per gossip batch
    #[serde(default = "default_max_gossip_batch")]
    pub max_gossip_batch: usize,
    
    /// Gossip fanout
    #[serde(default = "default_gossip_fanout")]
    pub gossip_fanout: u32,
}

fn default_anchor_interval() -> u64 {
    3
}

fn default_finality_anchors() -> u32 {
    3
}

fn default_max_gossip_batch() -> usize {
    1000
}

fn default_gossip_fanout() -> u32 {
    10
}

impl Default for ConsensusConfig {
    fn default() -> Self {
        Self {
            anchor_interval: default_anchor_interval(),
            finality_anchors: default_finality_anchors(),
            max_gossip_batch: default_max_gossip_batch(),
            gossip_fanout: default_gossip_fanout(),
        }
    }
}

/// Network configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Listen addresses
    #[serde(default = "default_listen_addresses")]
    pub listen_addresses: Vec<String>,
    
    /// Bootstrap nodes
    #[serde(default)]
    pub bootstrap_nodes: Vec<String>,
    
    /// Maximum peers
    #[serde(default = "default_max_peers")]
    pub max_peers: u32,
    
    /// Minimum peers
    #[serde(default = "default_min_peers")]
    pub min_peers: u32,
    
    /// Target peers
    #[serde(default = "default_target_peers")]
    pub target_peers: u32,
    
    /// Keepalive interval in seconds
    #[serde(default = "default_keepalive")]
    pub keepalive_interval: u64,
    
    /// Handshake timeout in seconds
    #[serde(default = "default_handshake_timeout")]
    pub handshake_timeout: u64,
}

fn default_listen_addresses() -> Vec<String> {
    vec![
        "/ip4/0.0.0.0/tcp/30333".to_string(),
        "/ip4/0.0.0.0/udp/30333/quic-v1".to_string(),
    ]
}

fn default_max_peers() -> u32 {
    50
}

fn default_min_peers() -> u32 {
    8
}

fn default_target_peers() -> u32 {
    25
}

fn default_keepalive() -> u64 {
    30
}

fn default_handshake_timeout() -> u64 {
    10
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            listen_addresses: default_listen_addresses(),
            bootstrap_nodes: Vec::new(),
            max_peers: default_max_peers(),
            min_peers: default_min_peers(),
            target_peers: default_target_peers(),
            keepalive_interval: default_keepalive(),
            handshake_timeout: default_handshake_timeout(),
        }
    }
}

/// Distribution protocol configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DistributionConfig {
    /// Maximum peers in swarm
    #[serde(default = "default_max_peers")]
    pub max_peers: u32,
    
    /// Seeding ratio (upload/download)
    #[serde(default = "default_seeding_ratio")]
    pub seeding_ratio: f64,
    
    /// Piece size in bytes
    #[serde(default = "default_piece_size")]
    pub piece_size: usize,
    
    /// DHT replication factor
    #[serde(default = "default_dht_replication")]
    pub dht_replication: u32,
}

fn default_seeding_ratio() -> f64 {
    2.0
}

fn default_piece_size() -> usize {
    256 * 1024 // 256 KB
}

fn default_dht_replication() -> u32 {
    20
}

impl Default for DistributionConfig {
    fn default() -> Self {
        Self {
            max_peers: default_max_peers(),
            seeding_ratio: default_seeding_ratio(),
            piece_size: default_piece_size(),
            dht_replication: default_dht_replication(),
        }
    }
}

/// Rate limits
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateLimits {
    /// Strings per second
    #[serde(default = "default_strings_per_second")]
    pub strings_per_second: u32,
    
    /// Gossip messages per second
    #[serde(default = "default_gossip_per_second")]
    pub gossip_messages_per_second: u32,
    
    /// RPC requests per second
    #[serde(default = "default_rpc_per_second")]
    pub rpc_requests_per_second: u32,
    
    /// Bandwidth bytes per second
    #[serde(default = "default_bandwidth")]
    pub bandwidth_bytes_per_second: u64,
}

fn default_strings_per_second() -> u32 {
    1000
}

fn default_gossip_per_second() -> u32 {
    100
}

fn default_rpc_per_second() -> u32 {
    1000
}

fn default_bandwidth() -> u64 {
    100 * 1024 * 1024 // 100 MB/s
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            strings_per_second: default_strings_per_second(),
            gossip_messages_per_second: default_gossip_per_second(),
            rpc_requests_per_second: default_rpc_per_second(),
            bandwidth_bytes_per_second: default_bandwidth(),
        }
    }
}

/// RPC configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RPCConfig {
    /// Enable RPC
    #[serde(default = "default_true")]
    pub enabled: bool,
    
    /// Listen address
    #[serde(default = "default_rpc_address")]
    pub address: String,
    
    /// Enable TLS
    #[serde(default)]
    pub tls_enabled: bool,
    
    /// TLS certificate path
    pub tls_cert: Option<String>,
    
    /// TLS key path
    pub tls_key: Option<String>,
    
    /// Enable CORS
    #[serde(default = "default_true")]
    pub cors_enabled: bool,
    
    /// CORS origins
    #[serde(default = "default_cors_origins")]
    pub cors_origins: Vec<String>,
}

fn default_rpc_address() -> String {
    "127.0.0.1:9933".to_string()
}

fn default_cors_origins() -> Vec<String> {
    vec!["*".to_string()]
}

impl Default for RPCConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            address: default_rpc_address(),
            tls_enabled: false,
            tls_cert: None,
            tls_key: None,
            cors_enabled: true,
            cors_origins: default_cors_origins(),
        }
    }
}

/// Storage configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Cache size in bytes
    #[serde(default = "default_cache_size")]
    pub cache_size: usize,
    
    /// Enable compression
    #[serde(default = "default_true")]
    pub compression: bool,
    
    /// Maximum open files
    #[serde(default = "default_max_open_files")]
    pub max_open_files: i32,
    
    /// WAL size in bytes
    #[serde(default = "default_wal_size")]
    pub wal_size: usize,
}

fn default_cache_size() -> usize {
    1024 * 1024 * 1024 // 1 GB
}

fn default_max_open_files() -> i32 {
    1000
}

fn default_wal_size() -> usize {
    256 * 1024 * 1024 // 256 MB
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            cache_size: default_cache_size(),
            compression: true,
            max_open_files: default_max_open_files(),
            wal_size: default_wal_size(),
        }
    }
}

/// Logging configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level
    #[serde(default = "default_log_level")]
    pub level: String,
    
    /// Log format
    #[serde(default = "default_log_format")]
    pub format: String,
    
    /// Log file path
    pub file: Option<String>,
    
    /// Color output
    #[serde(default = "default_true")]
    pub color: bool,
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "text".to_string()
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
            file: None,
            color: true,
        }
    }
}

/// Metrics configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Enable metrics
    #[serde(default = "default_true")]
    pub enabled: bool,
    
    /// Metrics address
    #[serde(default = "default_metrics_address")]
    pub address: String,
}

fn default_metrics_address() -> String {
    "127.0.0.1:9615".to_string()
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            address: default_metrics_address(),
        }
    }
}

/// Validator configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorConfig {
    /// Keys path
    #[serde(default = "default_keys_path")]
    pub keys_path: String,
    
    /// Key rotation interval in hours
    #[serde(default)]
    pub key_rotation_interval: u64,
    
    /// Geographic zone
    #[serde(default)]
    pub geo_zone: Option<GeoZone>,
}

fn default_keys_path() -> String {
    "./keys/validator".to_string()
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            keys_path: default_keys_path(),
            key_rotation_interval: 0,
            geo_zone: None,
        }
    }
}

