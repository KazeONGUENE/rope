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
    /// Master-node governance + ACL (added 2026-05-03)
    #[serde(default)]
    pub governance: GovernanceSettings,
    /// Deployer identity attestation (added 2026-05-03)
    #[serde(default)]
    pub deployer: DeployerSettings,
}

/// Master-node governance configuration. Loads the master-nodes.toml
/// registry and enforces ACL on `rope_suspendNode` / `rope_isolateNode`
/// / `rope_eraseNode` RPC methods.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceSettings {
    /// Path to the master-nodes.toml registry file
    pub master_nodes_file: String,
    /// Enforce ACL (set to false only for local testing)
    pub enforce: bool,
    /// Where to append signed governance actions
    pub log_path: String,
}

impl Default for GovernanceSettings {
    fn default() -> Self {
        Self {
            master_nodes_file: "/home/ubuntu/datachain-rope/deploy/config/master-nodes.toml"
                .to_string(),
            enforce: true,
            log_path: "~/.rope/governance.log".to_string(),
        }
    }
}

/// Deployer identity attestation. Bound to the node's keypair via the
/// `self_signature` field, which is computed by `rope identity sign-deployer`.
/// Exposed via `rope_nodeIdentity` RPC. Empty fields mean "not declared"; an
/// unsigned (empty `self_signature`) attestation is treated as "claim only,
/// not yet verifiable" and reported as such.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DeployerSettings {
    /// Deployer's hot wallet (EVM address, 0x-prefixed)
    #[serde(default)]
    pub wallet_address: String,
    /// Datawallet+ DID (`did:datachain:...`)
    #[serde(default)]
    pub did: String,
    /// ONCHAINID contract address
    #[serde(default)]
    pub onchainid: String,
    /// Family name + given names (natural person) or legal name (org)
    #[serde(default)]
    pub name: String,
    /// Organization name (only for legal-person deployers)
    #[serde(default)]
    pub organization: String,
    /// Incorporation number (only for legal-person deployers)
    #[serde(default)]
    pub incorporation: String,
    /// Postal address
    #[serde(default)]
    pub address: String,
    /// Email — must resolve via DID claim to be trusted
    #[serde(default)]
    pub email: String,
    /// ISO-3166 alpha-2 country code
    #[serde(default)]
    pub country: String,
    /// Hex Ed25519 signature over canonical JSON of the attestation, by
    /// this node's own keypair. Empty until `rope identity sign-deployer`
    /// has been run on the node.
    #[serde(default)]
    pub self_signature: String,
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
    /// Primary EVM execution-layer JSON-RPC URL. On a node co-located with
    /// Reth this is the local `http://127.0.0.1:8595`. On a consensus-only
    /// node with no local Reth, set this to a reachable public/edge endpoint
    /// (e.g. `https://erpc.datachain.network`).
    pub url: String,
    /// Ordered fallback endpoints, tried in sequence when the primary is
    /// unreachable at the transport level. This makes the EVM-state path
    /// explicit and self-healing instead of relying on an implicit failover
    /// heuristic. Empty by default (preserves single-endpoint deployments).
    #[serde(default)]
    pub fallback_urls: Vec<String>,
    /// Health check interval (seconds)
    pub health_interval_secs: u64,
    /// Max consecutive health check failures before marking unhealthy
    pub max_failures: u32,
    /// Optional WebSocket URL for Reth's `--ws` port. When set, the
    /// rope-node WSS server (`wss://ws.datachain.network` /
    /// `wss://ws.rope.network`) can bridge `eth_subscribe` /
    /// `eth_unsubscribe` requests upstream so that live push notifications
    /// (`eth_subscription`) reach connected clients unchanged. Env var
    /// `ROPE_RETH_WS_URL` overrides this field at runtime. Empty string
    /// disables the bridge (`eth_subscribe` returns a canonical JSON-RPC
    /// error instead of a dead subscription id).
    #[serde(default)]
    pub ws_url: Option<String>,
}

impl EvmBackendSettings {
    /// Ordered, de-duplicated endpoint list: primary first, then each
    /// fallback that is not already present. Always non-empty.
    pub fn endpoint_list(&self) -> Vec<String> {
        let mut urls = vec![self.url.clone()];
        for u in &self.fallback_urls {
            if !urls.contains(u) {
                urls.push(u.clone());
            }
        }
        urls
    }

    /// Resolve the upstream Reth WS URL for the subscription bridge.
    ///
    /// Precedence:
    ///   1. `ROPE_RETH_WS_URL` env var (empty string ⇒ explicit disable)
    ///   2. `[evm_backend].ws_url` from TOML (Some(empty) ⇒ explicit disable)
    ///   3. `ws://127.0.0.1:8547` (matches
    ///      `deploy/systemd/reth-rope.service --ws.port 8547`)
    ///
    /// Returns `None` only when the operator has explicitly disabled the
    /// bridge; in that case `eth_subscribe` returns a canonical JSON-RPC
    /// error instead of a dead subscription id.
    pub fn resolved_ws_url(&self) -> Option<String> {
        if let Ok(env) = std::env::var("ROPE_RETH_WS_URL") {
            let env = env.trim().to_string();
            return if env.is_empty() { None } else { Some(env) };
        }
        match self.ws_url.as_deref().map(str::trim) {
            Some("") => None,
            Some(u) => Some(u.to_string()),
            None => Some("ws://127.0.0.1:8547".to_string()),
        }
    }
}

impl Default for EvmBackendSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            // Production Reth listens on 8595 (was 8548 under Anvil).
            url: "http://127.0.0.1:8595".to_string(),
            fallback_urls: Vec::new(),
            health_interval_secs: 30,
            max_failures: 5,
            // Default matches `--ws.port 8547` in the production Reth unit.
            // Operators can override via `ROPE_RETH_WS_URL` env var or the
            // `[evm_backend].ws_url` TOML key.
            ws_url: None,
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
            governance: GovernanceSettings::default(),
            deployer: DeployerSettings::default(),
        }
    }

    /// Testnet configuration (chain ID 271829).
    ///
    /// Explicitly constructed instead of inheriting from `mainnet()`
    /// so that adding a new mainnet field never silently leaks a
    /// mainnet-only default into the testnet facade. In particular:
    ///
    /// * The default deployment topology is **a dedicated host** —
    ///   `rope-testnet-1` on DigitalOcean, per
    ///   `docs/design/testnet-parity-roadmap-2026-08-30.md` §3 and the
    ///   2026-08-31 dedicated-host decision. The testnet does NOT run
    ///   alongside mainnet on `new-blue`; consequently the ports below
    ///   mirror mainnet's natural layout so every ops script that
    ///   assumes "rope-node listens on :8545 / :8546 / :9001 / :9090
    ///   / :9000 / evm backend :8595" works on the testnet box
    ///   identically. If a future operator needs to co-host mainnet
    ///   and testnet on the same box, override the ports via a
    ///   `deploy/config/rope-testnet.toml` file rather than baking
    ///   collision-shifted ports into the default.
    /// * The EVM backend URL points at the testnet Reth engine on
    ///   `127.0.0.1:8595` (same natural port as mainnet Reth, but on
    ///   the dedicated testnet box so there is no collision).
    /// * `network.bootstrap_nodes` is empty by design — the testnet
    ///   is a single-writer facade in front of a dev-mode Reth and
    ///   does not yet have a peer swarm; adding the mainnet bootstrap
    ///   here would advertise cross-chain peer info.
    /// * `consensus.enabled = false` — the testnet runs in `relay`
    ///   mode in front of Reth's own block production; there is no
    ///   Testimony quorum on 271829 today.
    ///
    /// See `docs/design/rope-testnet-writer-facade.md` for the deploy
    /// topology and `docs/design/testnet-parity-roadmap-2026-08-30.md`
    /// §2.1 for the rationale.
    pub fn testnet() -> Self {
        Self {
            node: NodeSettings {
                name: "rope-testnet-node".to_string(),
                mode: NodeMode::Relay,
                chain_id: 271829,
                external_ip: None,
            },
            network: NetworkSettings {
                // Natural libp2p port. Testnet lives on a dedicated
                // host (rope-testnet-1), so there is no collision
                // with mainnet's :9000.
                listen_addr: "0.0.0.0:9000".to_string(),
                // Deliberately empty — see doc comment above.
                bootstrap_nodes: Vec::new(),
                max_peers: 50,
                enable_quic: true,
                enable_nat: true,
            },
            consensus: ConsensusSettings {
                // Testnet facade runs in relay mode; Reth (dev mode)
                // produces blocks. There is no Testimony quorum on
                // 271829 today.
                enabled: false,
                block_time_ms: 3000,
                min_testimonies: 1,
                ai_agents_enabled: false,
            },
            storage: StorageSettings {
                db_path: "~/.rope/testnet/db".to_string(),
                enable_compression: true,
                cache_size_mb: 256,
                pruning: PruningMode::Archive,
            },
            rpc: RpcSettings {
                enabled: true,
                // Natural mainnet-style ports. Testnet is single-tenant
                // on rope-testnet-1, so the "shift to avoid mainnet"
                // gymnastics that Phase 0 originally proposed is no
                // longer necessary.
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
                // Natural Prometheus port. See RPC note above.
                prometheus_addr: "127.0.0.1:9090".to_string(),
            },
            evm_backend: EvmBackendSettings {
                enabled: true,
                // Testnet Reth (dev mode) on the dedicated testnet
                // box uses the natural Reth HTTP port :8595. Mainnet
                // Reth uses the same port but lives on new-blue, so
                // there is no cross-box confusion.
                url: "http://127.0.0.1:8595".to_string(),
                fallback_urls: Vec::new(),
                health_interval_secs: 30,
                max_failures: 5,
                // Falls through to the ws://127.0.0.1:8547 default
                // via `EvmBackendSettings::resolved_ws_url`, or to
                // the `ROPE_RETH_WS_URL` env var if set.
                ws_url: None,
            },
            iot_gateway: IoTGatewaySettings::default(),
            ai_framework: AIFrameworkSettings::default(),
            governance: GovernanceSettings::default(),
            deployer: DeployerSettings::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard for `docs/design/testnet-parity-roadmap-2026-08-30.md`
    /// §2.1 and the 2026-08-31 dedicated-host decision.
    /// `NodeConfig::testnet()` MUST be explicitly constructed (not
    /// inherited from `mainnet()`) so that adding a new mainnet field
    /// never silently leaks a mainnet-only default into the testnet
    /// facade. It MUST use natural mainnet-style RPC ports (the
    /// testnet lives on a dedicated host, so no port shifting is
    /// needed) and MUST leave consensus disabled with an empty
    /// bootstrap list.
    #[test]
    fn testnet_config_uses_natural_ports_and_disabled_consensus() {
        let cfg = NodeConfig::testnet();

        assert_eq!(cfg.node.chain_id, 271829);
        assert_eq!(cfg.node.name, "rope-testnet-node");
        assert_eq!(cfg.node.mode, NodeMode::Relay);

        // RPC ports mirror mainnet's natural layout. This is safe
        // because the testnet lives on a dedicated box
        // (`rope-testnet-1`) that never runs mainnet rope-node.
        assert_eq!(cfg.rpc.http_addr, "127.0.0.1:8545");
        assert_eq!(cfg.rpc.ws_addr, "127.0.0.1:8546");
        assert_eq!(cfg.rpc.grpc_addr, "127.0.0.1:9001");
        assert_eq!(cfg.metrics.prometheus_addr, "127.0.0.1:9090");

        // EVM backend mirrors mainnet Reth's natural port (:8595).
        // On the dedicated testnet box this refers to the testnet
        // Reth engine; there is no cross-box collision.
        assert_eq!(cfg.evm_backend.url, "http://127.0.0.1:8595");

        // P2P uses the natural libp2p port.
        assert_eq!(cfg.network.listen_addr, "0.0.0.0:9000");

        // Testnet chain_id must be distinct from mainnet.
        assert_ne!(cfg.node.chain_id, 271828,
            "testnet chain_id must not equal mainnet chain_id (271828)");

        // No cross-chain peer info.
        assert!(cfg.network.bootstrap_nodes.is_empty(),
            "testnet must not advertise mainnet bootstrap peers");

        // Facade runs in relay mode in front of Reth dev-mode block
        // production.
        assert!(!cfg.consensus.enabled,
            "testnet consensus must be disabled (Reth dev-mode produces blocks)");

        // Testnet must never accidentally end up storing state in
        // mainnet's data dir.
        assert!(cfg.storage.db_path.contains("testnet"),
            "testnet db_path must be namespaced under `testnet` (got {})",
            cfg.storage.db_path);
    }

    /// Sanity: mainnet defaults must not have drifted while we were
    /// rewriting testnet. Guards `docs/design/testnet-parity-roadmap-2026-08-30.md`
    /// §"no mainnet wire drift" acceptance.
    #[test]
    fn mainnet_config_defaults_unchanged() {
        let cfg = NodeConfig::mainnet();
        assert_eq!(cfg.node.chain_id, 271828);
        assert_eq!(cfg.rpc.http_addr, "127.0.0.1:8545");
        assert_eq!(cfg.rpc.ws_addr, "127.0.0.1:8546");
        assert_eq!(cfg.rpc.grpc_addr, "127.0.0.1:9001");
        assert_eq!(cfg.metrics.prometheus_addr, "127.0.0.1:9090");
        assert_eq!(cfg.evm_backend.url, "http://127.0.0.1:8595");
        assert_eq!(cfg.network.listen_addr, "0.0.0.0:9000");
    }

    #[test]
    fn for_network_dispatches_correctly() {
        let m = NodeConfig::for_network("mainnet").expect("mainnet");
        assert_eq!(m.node.chain_id, 271828);
        let t = NodeConfig::for_network("testnet").expect("testnet");
        assert_eq!(t.node.chain_id, 271829);
        assert!(NodeConfig::for_network("nope").is_err());
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
