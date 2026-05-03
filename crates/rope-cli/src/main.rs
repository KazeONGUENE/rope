//! Datachain Rope CLI
//!
//! Command-line interface for running Rope nodes.

use clap::{Parser, Subcommand};
use libp2p::identity::Keypair as LibP2pKeypair;
use rope_crypto::keys::KeyPair;
use rope_node::{NodeConfig, RopeNode};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// JSON-RPC request structure
#[derive(Serialize)]
struct JsonRpcRequest {
    jsonrpc: String,
    method: String,
    params: Vec<serde_json::Value>,
    id: u64,
}

/// JSON-RPC response structure
#[derive(Deserialize)]
struct JsonRpcResponse {
    #[allow(dead_code)]
    jsonrpc: String,
    result: Option<serde_json::Value>,
    error: Option<JsonRpcError>,
    #[allow(dead_code)]
    id: u64,
}

/// JSON-RPC error
#[derive(Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

/// Simple RPC client for Datachain Rope
struct RpcClient {
    endpoint: String,
    client: reqwest::Client,
}

impl RpcClient {
    fn new(endpoint: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            client: reqwest::Client::new(),
        }
    }

    async fn call(
        &self,
        method: &str,
        params: Vec<serde_json::Value>,
    ) -> anyhow::Result<serde_json::Value> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
            id: 1,
        };

        let response = self
            .client
            .post(&self.endpoint)
            .json(&request)
            .send()
            .await?;

        let json_response: JsonRpcResponse = response.json().await?;

        if let Some(error) = json_response.error {
            anyhow::bail!("RPC error {}: {}", error.code, error.message);
        }

        json_response
            .result
            .ok_or_else(|| anyhow::anyhow!("No result in response"))
    }

    async fn get_chain_id(&self) -> anyhow::Result<u64> {
        let result = self.call("eth_chainId", vec![]).await?;
        let hex_str = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid chain ID response"))?;
        let chain_id = u64::from_str_radix(hex_str.trim_start_matches("0x"), 16)?;
        Ok(chain_id)
    }

    async fn get_block_number(&self) -> anyhow::Result<u64> {
        let result = self.call("eth_blockNumber", vec![]).await?;
        let hex_str = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid block number response"))?;
        let block_num = u64::from_str_radix(hex_str.trim_start_matches("0x"), 16)?;
        Ok(block_num)
    }

    async fn get_balance(&self, address: &str) -> anyhow::Result<u128> {
        let result = self
            .call(
                "eth_getBalance",
                vec![
                    serde_json::Value::String(address.to_string()),
                    serde_json::Value::String("latest".to_string()),
                ],
            )
            .await?;
        let hex_str = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid balance response"))?;
        let balance = u128::from_str_radix(hex_str.trim_start_matches("0x"), 16)?;
        Ok(balance)
    }

    async fn get_peer_count(&self) -> anyhow::Result<u64> {
        let result = self.call("net_peerCount", vec![]).await?;
        let hex_str = result
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid peer count response"))?;
        let count = u64::from_str_radix(hex_str.trim_start_matches("0x"), 16)?;
        Ok(count)
    }
}

const DEFAULT_RPC_ENDPOINT: &str = "https://erpc.datachain.network";

#[derive(Parser)]
#[command(name = "rope")]
#[command(author = "Datachain Foundation")]
#[command(version = "0.1.0")]
#[command(about = "Datachain Rope - Distributed Information Communication Protocol")]
#[command(long_about = r#"
Datachain Rope CLI - A revolutionary protocol inspired by DNA's double helix structure.

QUICK START:
  rope node --network mainnet    Start a relay node on mainnet
  rope query status              Check network status
  rope token balance [ADDRESS]   Check FAT token balance

NETWORK INFO:
  Chain ID:       271828 (0x425D4)
  RPC:            https://erpc.datachain.network
  Explorer:       https://dcscan.io
  WebSocket:      wss://ws.datachain.network

For more information: https://datachain.network/docs
"#)]
struct Cli {
    #[command(subcommand)]
    command: Commands,

    /// Enable verbose debug logging (set RUST_LOG=debug for more control)
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a Rope node (validator, relay, or seeder)
    ///
    /// Examples:
    ///   rope node                           Start relay node with defaults
    ///   rope node --mode validator          Start as validator node
    ///   rope node --network testnet         Connect to testnet
    ///   rope node -c custom.toml            Use custom config file
    #[command(after_help = "See https://datachain.network/docs/node for full setup guide")]
    Node {
        /// Configuration file path (TOML format)
        #[arg(short, long, default_value = "rope.toml")]
        config: PathBuf,

        /// Data directory for blockchain state and keys
        #[arg(short, long, default_value = "~/.rope")]
        data_dir: PathBuf,

        /// Node mode: validator (requires stake), relay (P2P routing), or seeder (data distribution)
        #[arg(short, long, default_value = "relay", value_parser = ["validator", "relay", "seeder"])]
        mode: String,

        /// Network to connect to: mainnet (Chain ID 271828) or testnet
        #[arg(short, long, default_value = "mainnet", value_parser = ["mainnet", "testnet"])]
        network: String,
    },

    /// Generate cryptographic keypairs for node identity
    ///
    /// Examples:
    ///   rope keygen                         Generate standard Ed25519 keys
    ///   rope keygen --quantum               Generate post-quantum keys (Dilithium3)
    ///   rope keygen -o /path/to/keys        Specify output directory
    #[command(after_help = "Keys are stored in PEM format. Backup securely!")]
    Keygen {
        /// Output directory for generated keys
        #[arg(short, long, default_value = "~/.rope/keys")]
        output: PathBuf,

        /// Generate quantum-resistant keys using CRYSTALS-Dilithium3
        #[arg(long)]
        quantum: bool,
    },

    /// Display local node information and configuration
    ///
    /// Examples:
    ///   rope info                           Show default node info
    ///   rope info -d /custom/path           Show info for specific data directory
    Info {
        /// Data directory to inspect
        #[arg(short, long, default_value = "~/.rope")]
        data_dir: PathBuf,
    },

    /// Initialize a new genesis federation configuration
    ///
    /// Examples:
    ///   rope genesis                        Create genesis with 21 validators
    ///   rope genesis -v 7                   Create with 7 validators
    ///   rope genesis --chain-id 314159     Use custom chain ID
    #[command(after_help = "Genesis file defines the initial network state")]
    Genesis {
        /// Number of initial validators (typically 7, 13, or 21 for BFT)
        #[arg(short, long, default_value = "21")]
        validators: u32,

        /// Chain ID for the network (default: 271828 for Datachain Rope)
        #[arg(long, default_value = "271828")]
        chain_id: u64,

        /// Output file path for genesis configuration
        #[arg(short, long, default_value = "genesis.json")]
        output: PathBuf,
    },

    /// Query network state and information via RPC
    ///
    /// Examples:
    ///   rope query status                   Show network health and block height
    ///   rope query peers                    List connected peer count
    ///   rope query validators               Show active validator set
    ///   rope query string `<ID>`            Lookup a specific string by ID
    Query {
        #[command(subcommand)]
        query: QueryCommands,
    },

    /// FAT token operations (balance, transfer)
    ///
    /// Examples:
    ///   rope token balance 0x1234...        Check balance of address
    ///   rope token transfer 0xABC... 100    Transfer 100 FAT tokens
    Token {
        #[command(subcommand)]
        token: TokenCommands,
    },

    /// Display version and build information
    Version,

    /// Extract peer ID from node key file (useful for bootstrap configuration)
    ///
    /// Examples:
    ///   rope peer-id -k ~/.rope/keys/node.key
    ///   rope peer-id -k node.key --ip 1.2.3.4 --port 9000
    #[command(name = "peer-id")]
    PeerId {
        /// Path to the node private key file
        #[arg(short, long)]
        key: PathBuf,

        /// Optional IP address for generating complete multiaddr
        #[arg(long)]
        ip: Option<String>,

        /// P2P port number (default: 9000)
        #[arg(long, default_value = "9000")]
        port: u16,
    },

    /// Manage node deployer identity (Datawallet+ DID + ONCHAINID attestation)
    ///
    /// Every node carries a deployer attestation linking it to a real
    /// person or organization. See master-node-governance.mdc for the
    /// full data model.
    Identity {
        #[command(subcommand)]
        action: IdentityCommands,
    },

    /// Master-node governance actions (suspend / isolate / erase nodes)
    ///
    /// Mutating actions require a signed payload using a key listed in
    /// master-nodes.toml under `founder_keys` or `master_nodes[*].pubkey_ed25519`.
    Governance {
        #[command(subcommand)]
        action: GovernanceCommands,
    },

    /// Deploy a new Datachain Rope node on a supported cloud provider
    ///
    /// Examples:
    ///   rope deploy local             rpc-slot          Run a local rope-node + Reth in Docker
    ///   rope deploy exoscale          witness           Provision a witness on Foundation Exoscale
    ///   rope deploy digitalocean      community-node    Provision a community node on DigitalOcean
    ///
    /// Phase D MVP: `local` works today. Cloud provisioning requires the
    /// `rope-deployer` service to be reachable; see EXOSCALE_AS_A_SERVICE.md.
    Deploy {
        /// Cloud provider (exoscale | digitalocean | local)
        #[arg(value_parser = ["exoscale", "digitalocean", "local"])]
        provider: String,

        /// Node kind (rpc-slot | witness | community-node | databox)
        #[arg(value_parser = ["rpc-slot", "witness", "community-node", "databox"])]
        kind: String,

        /// Region (provider-specific, e.g. ch-gva-2 / fra1)
        #[arg(long)]
        region: Option<String>,

        /// Instance size (provider-specific, e.g. medium / s-2vcpu-4gb)
        #[arg(long, default_value = "medium")]
        size: String,

        /// Datawallet+ DID claim file (required for non-foundation deployers)
        #[arg(long)]
        identity: Option<PathBuf>,

        /// Dry run — print what would be provisioned without calling the cloud API
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Subcommand)]
enum IdentityCommands {
    /// Generate a new founder-level Ed25519 key (write to file, print pubkey)
    ///
    /// Run once on the founder's secure machine. The resulting pubkey
    /// must be added to deploy/config/master-nodes.toml under
    /// `founder.founder_keys` and the file rsync'd to all production nodes.
    InitFounder {
        /// Output path for the private key (32 bytes raw)
        #[arg(short, long, default_value = "~/.rope/founder.key")]
        output: PathBuf,
    },

    /// Sign this node's [deployer] block with a founder/master key
    ///
    /// Reads the [deployer] section of the given config file, computes the
    /// canonical JSON, signs it with the supplied key, and writes the
    /// resulting hex signature back to the file's `self_signature` field.
    SignDeployer {
        /// Path to rope-production.toml (or rope-witness.toml)
        #[arg(short, long)]
        config: PathBuf,

        /// Path to the signing key (32 bytes raw, e.g. ~/.rope/founder.key)
        #[arg(short, long)]
        key: PathBuf,
    },

    /// Show the deployer identity recorded in a config file (or queried via RPC)
    Show {
        /// Path to rope-production.toml (omit to query via --rpc)
        #[arg(short, long)]
        config: Option<PathBuf>,

        /// RPC endpoint (default: production)
        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,

        /// Optional node_id to query a remote node's deployer attestation
        #[arg(long)]
        node_id: Option<String>,
    },

    /// Verify a deployer attestation (signature, founder/master ACL)
    Verify {
        /// Path to rope-production.toml
        #[arg(short, long)]
        config: PathBuf,
    },
}

#[derive(Subcommand)]
enum GovernanceCommands {
    /// List the current master-node registry (read-only, no auth)
    ListMasters {
        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,
    },

    /// Show full governance info: registry + recent log entries
    Info {
        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,
    },

    /// Suspend a node (refuse new connections for `ttl` seconds).
    /// Requires a master-node OR founder signing key.
    Suspend {
        /// Target node_id (hex)
        #[arg(long)]
        node_id: String,

        /// Reason recorded in the governance log
        #[arg(long)]
        reason: String,

        /// Suspension TTL in seconds (default: 1 hour)
        #[arg(long, default_value = "3600")]
        ttl: u64,

        /// Path to a 32-byte raw signing key (founder or master)
        #[arg(short, long)]
        key: PathBuf,

        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,
    },

    /// Isolate a node permanently (drops connections + ignores future ones).
    /// Requires a FOUNDER signing key.
    Isolate {
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        reason: String,
        #[arg(short, long)]
        key: PathBuf,
        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,
    },

    /// Erase a node from the recognized set (testimonies no longer counted).
    /// Requires a FOUNDER signing key.
    Erase {
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        reason: String,
        #[arg(short, long)]
        key: PathBuf,
        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,
    },

    /// Anchor THIS node's signed [deployer] attestation onto the deployer's
    /// personal ledger (which lives on the global Datachain Rope lattice ==
    /// main Rope ledger). Useful after re-signing or for an audit refresh.
    ///
    /// No external auth — the receiving node already holds the founder-signed
    /// `self_signature` in its own config; the RPC call only forwards what's
    /// already on disk.
    AnchorDeployer {
        /// Force re-anchor even if a marker already exists locally
        #[arg(long)]
        force: bool,

        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,
    },

    /// Show how many deployer attestations have been anchored on a wallet's
    /// personal ledger. When `--wallet` is omitted the call returns the
    /// remote node's own [deployer].wallet_address.
    ListDeployerAttestations {
        /// Wallet address (0x...) — defaults to the remote node's own
        #[arg(long)]
        wallet: Option<String>,

        #[arg(long, default_value = DEFAULT_RPC_ENDPOINT)]
        rpc: String,
    },
}

#[derive(Subcommand)]
enum QueryCommands {
    /// Lookup a string in the lattice by its ID
    ///
    /// Example: rope query string 0x1234567890abcdef...
    String {
        /// String ID in hex format (64 characters)
        #[arg(value_name = "STRING_ID")]
        id: String,
    },

    /// Display current network status including block height and peers
    ///
    /// Example: rope query status
    Status,

    /// Show connected peer information
    ///
    /// Example: rope query peers
    Peers,

    /// List the current active validator set
    ///
    /// Example: rope query validators
    Validators,
}

#[derive(Subcommand)]
enum TokenCommands {
    /// Check FAT token balance for an address
    ///
    /// Example: rope token balance 0x742d35Cc6634C0532925a3b844Bc9e7595f12345
    Balance {
        /// Wallet address in hex format (with or without 0x prefix)
        #[arg(value_name = "ADDRESS")]
        address: String,
    },

    /// Transfer FAT tokens to another address (requires wallet)
    ///
    /// Example: rope token transfer 0xRecipient... 100
    ///
    /// Note: This command shows transfer instructions. For actual transfers,
    /// use Datawallet+ app or MetaMask with Datachain Rope network configured.
    Transfer {
        /// Recipient wallet address
        #[arg(value_name = "TO_ADDRESS")]
        to: String,

        /// Amount of FAT tokens to transfer
        #[arg(value_name = "AMOUNT")]
        amount: u64,
    },
}

fn init_logging(verbose: bool) {
    let env_filter = if verbose {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("debug"))
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"))
    };

    tracing_subscriber::registry()
        .with(env_filter)
        .with(
            tracing_subscriber::fmt::layer()
                .with_target(true)
                .with_thread_ids(false)
                .with_file(false),
        )
        .init();
}

fn expand_path(path: &PathBuf) -> PathBuf {
    if let Some(path_str) = path.to_str() {
        if path_str.starts_with("~") {
            if let Some(home) = dirs::home_dir() {
                return home.join(&path_str[2..]);
            }
        }
    }
    path.clone()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_logging(cli.verbose);

    match cli.command {
        Commands::Node {
            config,
            data_dir,
            mode,
            network,
        } => {
            let config_path = expand_path(&config);
            let data_dir = expand_path(&data_dir);

            tracing::info!("╔══════════════════════════════════════════════════════════════╗");
            tracing::info!("║           DATACHAIN ROPE NODE v0.1.0                         ║");
            tracing::info!("║   Distributed Information Communication Protocol            ║");
            tracing::info!("╚══════════════════════════════════════════════════════════════╝");
            tracing::info!("");
            tracing::info!("Network: {}", network);
            tracing::info!("Mode: {}", mode);
            tracing::info!("Config: {:?}", config_path);
            tracing::info!("Data: {:?}", data_dir);

            // Load or create config
            let mut node_config: NodeConfig = if config_path.exists() {
                let content = std::fs::read_to_string(&config_path)?;
                toml::from_str(&content)?
            } else {
                tracing::info!("Config not found, using defaults for {}", network);
                NodeConfig::for_network(&network)?
            };

            // Override mode from CLI
            node_config.node.mode = match mode.to_lowercase().as_str() {
                "validator" => rope_node::config::NodeMode::Validator,
                "relay" => rope_node::config::NodeMode::Relay,
                "seeder" => rope_node::config::NodeMode::Seeder,
                _ => {
                    tracing::warn!("Unknown mode '{}', defaulting to relay", mode);
                    rope_node::config::NodeMode::Relay
                }
            };

            // Create data directory
            std::fs::create_dir_all(&data_dir)?;

            // Start node
            let mut node = RopeNode::new(node_config, data_dir).await?;
            node.run().await?;
        }

        Commands::Keygen { output, quantum } => {
            let output_dir = expand_path(&output);
            std::fs::create_dir_all(&output_dir)?;

            tracing::info!("Generating keypair...");

            let keypair = if quantum {
                tracing::info!("Using hybrid quantum-resistant keys (Ed25519 + Dilithium3)");
                KeyPair::generate_hybrid()?
            } else {
                tracing::info!("Using classical Ed25519 keys");
                KeyPair::generate()?
            };

            let node_id = keypair.node_id();

            // Save keys
            let priv_key_path = output_dir.join("node.key");
            let pub_key_path = output_dir.join("node.pub");
            let id_path = output_dir.join("node.id");

            std::fs::write(&priv_key_path, keypair.private_key_bytes())?;
            std::fs::write(&pub_key_path, keypair.public_key_bytes())?;
            std::fs::write(&id_path, hex::encode(node_id))?;

            println!("Keypair generated successfully!");
            println!("Node ID: {}", hex::encode(node_id));
            println!("Private key: {:?}", priv_key_path);
            println!("Public key: {:?}", pub_key_path);
        }

        Commands::Info { data_dir } => {
            let data_dir = expand_path(&data_dir);

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║                  DATACHAIN ROPE INFO                         ║");
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!("");
            println!("Version: 0.1.0");
            println!("Protocol: Datachain Rope (String Lattice)");
            println!("Consensus: Testimony Protocol");
            println!("Cryptography: Ed25519 + Dilithium3 (PQ-resistant)");
            println!("");
            println!("Data directory: {:?}", data_dir);

            // Check for keys
            let id_path = data_dir.join("keys/node.id");
            if id_path.exists() {
                let node_id = std::fs::read_to_string(&id_path)?;
                println!("Node ID: {}", node_id);
            } else {
                println!("Node ID: Not configured (run 'rope keygen' first)");
            }

            println!("");
            println!("Network Info:");
            println!("  Mainnet Chain ID: 271828");
            println!("  Testnet Chain ID: 271829");
            println!("  RPC: https://erpc.datachain.network");
            println!("  Explorer: https://dcscan.io");
            println!("");
            println!("https://datachain.network");
        }

        Commands::Genesis {
            validators,
            chain_id,
            output,
        } => {
            let output_path = expand_path(&output);

            tracing::info!("Generating genesis with {} validators...", validators);

            let genesis = rope_node::genesis::generate_genesis(validators, chain_id)?;
            let genesis_json = serde_json::to_string_pretty(&genesis)?;

            std::fs::write(&output_path, &genesis_json)?;

            println!("Genesis generated successfully!");
            println!("Output: {:?}", output_path);
            println!("Chain ID: {}", chain_id);
            println!("Validators: {}", validators);
            println!("Genesis hash: {}", hex::encode(&genesis.genesis_hash));
        }

        Commands::Query { query } => {
            let rpc = RpcClient::new(DEFAULT_RPC_ENDPOINT);

            match query {
                QueryCommands::String { id } => {
                    println!("Querying string: {}", id);
                    println!("String query not yet available via JSON-RPC");
                    println!("Use the native Rope API for string queries");
                }
                QueryCommands::Status => {
                    println!("╔══════════════════════════════════════════════════════════════╗");
                    println!("║                  NETWORK STATUS                              ║");
                    println!("╚══════════════════════════════════════════════════════════════╝");
                    println!("");

                    match rpc.get_chain_id().await {
                        Ok(chain_id) => println!("Chain ID:     {} (0x{:X})", chain_id, chain_id),
                        Err(e) => println!("Chain ID:     Error - {}", e),
                    }

                    match rpc.get_block_number().await {
                        Ok(block) => println!("Block Height: {}", block),
                        Err(e) => println!("Block Height: Error - {}", e),
                    }

                    match rpc.get_peer_count().await {
                        Ok(peers) => println!("Peer Count:   {}", peers),
                        Err(e) => println!("Peer Count:   Error - {}", e),
                    }

                    println!("");
                    println!("RPC Endpoint: {}", DEFAULT_RPC_ENDPOINT);
                }
                QueryCommands::Peers => {
                    println!("Connected Peers:");
                    match rpc.get_peer_count().await {
                        Ok(count) => {
                            println!("Total connected peers: {}", count);
                            println!("");
                            println!("(Detailed peer list requires native Rope API)");
                        }
                        Err(e) => println!("Error getting peer count: {}", e),
                    }
                }
                QueryCommands::Validators => {
                    println!("Validator Set:");
                    println!("(Validator queries require native Rope API)");
                    println!("");
                    println!("Datachain Rope uses 21 rotating validators");
                    println!("See https://dcscan.io/validators for current set");
                }
            }
        }

        Commands::Token { token } => {
            let rpc = RpcClient::new(DEFAULT_RPC_ENDPOINT);

            match token {
                TokenCommands::Balance { address } => {
                    // Ensure address has 0x prefix
                    let addr = if address.starts_with("0x") {
                        address.clone()
                    } else {
                        format!("0x{}", address)
                    };

                    println!("╔══════════════════════════════════════════════════════════════╗");
                    println!("║                  TOKEN BALANCE                               ║");
                    println!("╚══════════════════════════════════════════════════════════════╝");
                    println!("");
                    println!("Address: {}", addr);

                    match rpc.get_balance(&addr).await {
                        Ok(balance_wei) => {
                            let balance_fat = balance_wei as f64 / 1e18;
                            println!("Balance: {:.6} FAT", balance_fat);
                            println!("         ({} wei)", balance_wei);
                        }
                        Err(e) => println!("Error: {}", e),
                    }
                }
                TokenCommands::Transfer { to, amount } => {
                    println!("╔══════════════════════════════════════════════════════════════╗");
                    println!("║                  TOKEN TRANSFER                              ║");
                    println!("╚══════════════════════════════════════════════════════════════╝");
                    println!("");
                    println!("To:     {}", to);
                    println!("Amount: {} FAT", amount);
                    println!("");
                    println!("Transfer requires wallet signing.");
                    println!("Use Datawallet+ app or web interface at https://datawallet.plus");
                    println!("");
                    println!("Or use MetaMask with:");
                    println!("  Network: Datachain Rope");
                    println!("  Chain ID: 271828");
                    println!("  RPC: https://erpc.datachain.network");
                }
            }
        }

        Commands::Version => {
            println!("Datachain Rope v0.1.0");
            println!("Build: release");
            println!("Rust: {}", rustc_version_runtime::version());
            println!("");
            println!("Features:");
            println!("  - String Lattice (DNA-inspired DAG)");
            println!("  - Testimony Consensus Protocol");
            println!("  - Organic Encryption System (OES)");
            println!("  - Hybrid Quantum-Resistant Cryptography");
            println!("  - DC FAT Native Token");
            println!("  - AI Testimony Agents");
        }

        Commands::PeerId { key, ip, port } => {
            let key_path = expand_path(&key);

            if !key_path.exists() {
                anyhow::bail!("Key file not found: {:?}", key_path);
            }

            let key_bytes = std::fs::read(&key_path)?;
            if key_bytes.len() < 32 {
                anyhow::bail!("Key file too short, need at least 32 bytes");
            }

            let seed: [u8; 32] = key_bytes[..32].try_into()?;
            let keypair = LibP2pKeypair::ed25519_from_bytes(seed)
                .map_err(|e| anyhow::anyhow!("Invalid seed: {:?}", e))?;
            let peer_id = keypair.public().to_peer_id();

            println!("╔══════════════════════════════════════════════════════════════╗");
            println!("║              DATACHAIN ROPE PEER ID                          ║");
            println!("╚══════════════════════════════════════════════════════════════╝");
            println!("");
            println!("Peer ID: {}", peer_id);
            println!("");

            if let Some(ip_addr) = ip {
                println!(
                    "Multiaddr (TCP):  /ip4/{}/tcp/{}/p2p/{}",
                    ip_addr, port, peer_id
                );
                println!(
                    "Multiaddr (QUIC): /ip4/{}/udp/{}/quic-v1/p2p/{}",
                    ip_addr, port, peer_id
                );
                println!("");
                println!("Add to bootstrap_nodes in config:");
                println!("  \"/ip4/{}/tcp/{}/p2p/{}\"", ip_addr, port, peer_id);
            } else {
                println!(
                    "Multiaddr (localhost TCP):  /ip4/127.0.0.1/tcp/{}/p2p/{}",
                    port, peer_id
                );
                println!("");
                println!("Use --ip <IP_ADDRESS> for full multiaddr");
            }
        }

        Commands::Identity { action } => identity::run(action).await?,
        Commands::Governance { action } => governance::run(action).await?,
        Commands::Deploy {
            provider,
            kind,
            region,
            size,
            identity,
            dry_run,
        } => {
            deploy::run(deploy::DeployArgs {
                provider,
                kind,
                region,
                size,
                identity,
                dry_run,
            })
            .await?
        }
    }

    Ok(())
}

// ===== identity / governance / deploy submodules =====

mod identity {
    use super::{expand_path, IdentityCommands, RpcClient};
    use anyhow::Context;
    use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
    use serde::{Deserialize, Serialize};
    use std::path::PathBuf;

    /// Subset of NodeConfig we need to read/write `[deployer]` without
    /// requiring rope-node as a dep.
    #[derive(Debug, Default, Clone, Serialize, Deserialize)]
    pub struct DeployerToml {
        #[serde(default)]
        pub wallet_address: String,
        #[serde(default)]
        pub did: String,
        #[serde(default)]
        pub onchainid: String,
        #[serde(default)]
        pub name: String,
        #[serde(default)]
        pub organization: String,
        #[serde(default)]
        pub incorporation: String,
        #[serde(default)]
        pub address: String,
        #[serde(default)]
        pub email: String,
        #[serde(default)]
        pub country: String,
        #[serde(default)]
        pub self_signature: String,
    }

    pub async fn run(action: IdentityCommands) -> anyhow::Result<()> {
        match action {
            IdentityCommands::InitFounder { output } => init_founder(output).await,
            IdentityCommands::SignDeployer { config, key } => sign_deployer(config, key).await,
            IdentityCommands::Show {
                config,
                rpc,
                node_id,
            } => show(config, rpc, node_id).await,
            IdentityCommands::Verify { config } => verify(config).await,
        }
    }

    async fn init_founder(output: PathBuf) -> anyhow::Result<()> {
        use rand::RngCore;
        let path = expand_path(&output);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if path.exists() {
            anyhow::bail!(
                "Refusing to overwrite existing key at {:?} — move it aside first",
                path
            );
        }
        let mut secret = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut secret);
        let signing = SigningKey::from_bytes(&secret);
        let pubkey = signing.verifying_key();
        std::fs::write(&path, secret)?;
        // Restrict perms (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        println!("=== Founder key generated ===");
        println!();
        println!("Private key file: {:?}", path);
        println!("Public key (hex): {}", hex::encode(pubkey.to_bytes()));
        println!();
        println!("NEXT STEPS:");
        println!("  1. Add the public key to:");
        println!("       deploy/config/master-nodes.toml");
        println!("     under [founder] founder_keys = [...]");
        println!("  2. Commit + push to git");
        println!("  3. rsync the file to all production nodes:");
        println!("       /home/ubuntu/datachain-rope/deploy/config/master-nodes.toml");
        println!();
        println!("KEEP THE PRIVATE KEY OFFLINE OR ON A HARDWARE WALLET WHEN POSSIBLE.");
        Ok(())
    }

    async fn sign_deployer(config: PathBuf, key: PathBuf) -> anyhow::Result<()> {
        let cfg_path = expand_path(&config);
        let key_path = expand_path(&key);
        let body = std::fs::read_to_string(&cfg_path)
            .with_context(|| format!("could not read {:?}", cfg_path))?;
        let top: toml::Value = toml::from_str(&body)?;
        let dep_value = top
            .get("deployer")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("[deployer] section missing in {:?}", cfg_path))?;
        let mut dep: DeployerToml = dep_value.try_into()?;

        // Sign over canonical JSON of the attestation MINUS self_signature
        dep.self_signature = String::new();
        let json = serde_json::to_value(&dep)?;
        let canonical = canonical_json(&json);

        let key_bytes = std::fs::read(&key_path)?;
        if key_bytes.len() < 32 {
            anyhow::bail!("signing key file too short (need 32 bytes raw seed)");
        }
        let seed: [u8; 32] = key_bytes[..32].try_into()?;
        let signing = SigningKey::from_bytes(&seed);
        let sig = signing.sign(canonical.as_bytes());
        let sig_hex = hex::encode(sig.to_bytes());

        // Surgically edit only the `self_signature = "..."` line, preserving
        // all other formatting/comments. We look for the LAST occurrence of
        // `self_signature` after the `[deployer]` section header.
        let new_body = patch_self_signature(&body, &sig_hex)?;
        std::fs::write(&cfg_path, new_body)?;

        println!("=== Deployer attestation signed ===");
        println!();
        println!("config:    {:?}", cfg_path);
        println!(
            "signer:    {}",
            hex::encode(signing.verifying_key().to_bytes())
        );
        println!("signature: {}", sig_hex);
        Ok(())
    }

    /// Replace the value of `self_signature = "..."` inside the `[deployer]`
    /// section (and only that section) with `new_sig`. Preserves all other
    /// formatting and comments.
    fn patch_self_signature(body: &str, new_sig: &str) -> anyhow::Result<String> {
        let mut out = String::with_capacity(body.len() + 32);
        let mut in_deployer = false;
        let mut patched = false;
        for line in body.lines() {
            let trimmed = line.trim_start();
            if let Some(rest) = trimmed.strip_prefix('[') {
                let header = rest.split(']').next().unwrap_or("");
                in_deployer = header.trim() == "deployer";
            }
            if in_deployer && !patched {
                if let Some(rest) = trimmed.strip_prefix("self_signature") {
                    if rest.trim_start().starts_with('=') {
                        // Preserve indent prefix
                        let indent_len = line.len() - trimmed.len();
                        let indent = &line[..indent_len];
                        out.push_str(&format!("{indent}self_signature  = \"{new_sig}\"\n"));
                        patched = true;
                        continue;
                    }
                }
            }
            out.push_str(line);
            out.push('\n');
        }
        if !patched {
            anyhow::bail!(
                "Could not locate `self_signature` line within [deployer] section. \
                 Please add a `self_signature = \"\"` line under [deployer] and re-run."
            );
        }
        // Trim accidental trailing newline if input did not have one
        if !body.ends_with('\n') && out.ends_with('\n') {
            out.pop();
        }
        Ok(out)
    }

    async fn show(
        config: Option<PathBuf>,
        rpc: String,
        node_id: Option<String>,
    ) -> anyhow::Result<()> {
        if let Some(cfg) = config {
            let body = std::fs::read_to_string(expand_path(&cfg))?;
            let top: toml::Value = toml::from_str(&body)?;
            let dep = top
                .get("deployer")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("[deployer] missing"))?;
            println!("{}", toml::to_string_pretty(&dep)?);
        } else {
            let client = RpcClient::new(&rpc);
            let params = match node_id {
                Some(id) => vec![serde_json::Value::String(id)],
                None => vec![],
            };
            let res = client.call("rope_nodeIdentity", params).await?;
            println!("{}", serde_json::to_string_pretty(&res)?);
        }
        Ok(())
    }

    async fn verify(config: PathBuf) -> anyhow::Result<()> {
        let cfg_path = expand_path(&config);
        let body = std::fs::read_to_string(&cfg_path)?;
        let top: toml::Value = toml::from_str(&body)?;
        let dep_value = top
            .get("deployer")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("[deployer] missing"))?;
        let dep: DeployerToml = dep_value.try_into()?;
        if dep.self_signature.is_empty() {
            println!("UNSIGNED: deployer attestation has no self_signature");
            return Ok(());
        }
        // We can't fully verify without the signer's pubkey, but we can
        // re-canonicalize and report the bytes that should have been signed.
        let mut for_sig = dep.clone();
        for_sig.self_signature = String::new();
        let json = serde_json::to_value(&for_sig)?;
        let canonical = canonical_json(&json);
        let sig_bytes =
            hex::decode(dep.self_signature.trim_start_matches("0x")).unwrap_or_default();
        if sig_bytes.len() != 64 {
            println!("MALFORMED: self_signature is not 64 bytes hex");
            return Ok(());
        }
        println!("=== Verifiable bytes (canonical JSON of [deployer] minus self_signature) ===");
        println!();
        println!("{canonical}");
        println!();
        println!("signature: {}", dep.self_signature);
        println!();
        println!(
            "To fully verify, look up the signing key in master-nodes.toml \
             under master_nodes[*].pubkey_ed25519 or founder.founder_keys, \
             then run `ed25519 verify` against the bytes above."
        );
        // Best-effort cross-check against any pubkey in master-nodes.toml
        if let Some(parent) = cfg_path.parent() {
            let mn = parent.join("master-nodes.toml");
            if mn.exists() {
                let mn_body = std::fs::read_to_string(&mn)?;
                let mn_top: toml::Value = toml::from_str(&mn_body)?;
                let mut sig_arr = [0u8; 64];
                sig_arr.copy_from_slice(&sig_bytes);
                let sig = ed25519_dalek::Signature::from_bytes(&sig_arr);
                let mut keys: Vec<(String, String)> = Vec::new();
                if let Some(arr) = mn_top.get("master_nodes").and_then(|v| v.as_array()) {
                    for entry in arr {
                        if let (Some(slot), Some(pk)) = (
                            entry.get("slot").and_then(|v| v.as_str()),
                            entry.get("pubkey_ed25519").and_then(|v| v.as_str()),
                        ) {
                            keys.push((format!("master:{slot}"), pk.to_string()));
                        }
                    }
                }
                if let Some(arr) = mn_top
                    .get("founder")
                    .and_then(|v| v.get("founder_keys"))
                    .and_then(|v| v.as_array())
                {
                    for (i, entry) in arr.iter().enumerate() {
                        if let Some(pk) = entry.as_str() {
                            keys.push((format!("founder[{i}]"), pk.to_string()));
                        }
                    }
                }
                for (label, pk_hex) in &keys {
                    let bytes = hex::decode(pk_hex.trim_start_matches("0x")).unwrap_or_default();
                    if bytes.len() != 32 {
                        continue;
                    }
                    let mut pk_arr = [0u8; 32];
                    pk_arr.copy_from_slice(&bytes);
                    if let Ok(vk) = VerifyingKey::from_bytes(&pk_arr) {
                        if vk.verify(canonical.as_bytes(), &sig).is_ok() {
                            println!("MATCH: signed by {label} ({pk_hex})");
                            return Ok(());
                        }
                    }
                }
                println!("NO MATCH: tried {} keys from master-nodes.toml", keys.len());
            }
        }
        Ok(())
    }

    pub fn canonical_json(v: &serde_json::Value) -> String {
        // Simple canonical form: sort object keys.
        use serde_json::Value;
        fn write(v: &Value, out: &mut String) {
            match v {
                Value::Null => out.push_str("null"),
                Value::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
                Value::Number(n) => out.push_str(&n.to_string()),
                Value::String(s) => {
                    out.push('"');
                    for c in s.chars() {
                        match c {
                            '"' => out.push_str("\\\""),
                            '\\' => out.push_str("\\\\"),
                            '\n' => out.push_str("\\n"),
                            '\r' => out.push_str("\\r"),
                            '\t' => out.push_str("\\t"),
                            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
                            c => out.push(c),
                        }
                    }
                    out.push('"');
                }
                Value::Array(a) => {
                    out.push('[');
                    for (i, x) in a.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        write(x, out);
                    }
                    out.push(']');
                }
                Value::Object(o) => {
                    let mut keys: Vec<&String> = o.keys().collect();
                    keys.sort();
                    out.push('{');
                    for (i, k) in keys.iter().enumerate() {
                        if i > 0 {
                            out.push(',');
                        }
                        write(&Value::String((*k).clone()), out);
                        out.push(':');
                        write(&o[*k], out);
                    }
                    out.push('}');
                }
            }
        }
        let mut s = String::new();
        write(v, &mut s);
        s
    }
}

mod governance {
    use super::{expand_path, GovernanceCommands, RpcClient};
    use crate::identity::canonical_json;
    use chrono::Utc;
    use ed25519_dalek::{Signer, SigningKey};
    use std::path::PathBuf;
    use uuid::Uuid;

    pub async fn run(action: GovernanceCommands) -> anyhow::Result<()> {
        match action {
            GovernanceCommands::ListMasters { rpc } => {
                let client = RpcClient::new(&rpc);
                let res = client.call("rope_listMasterNodes", vec![]).await?;
                println!("{}", serde_json::to_string_pretty(&res)?);
            }
            GovernanceCommands::Info { rpc } => {
                let client = RpcClient::new(&rpc);
                let res = client.call("rope_governanceInfo", vec![]).await?;
                println!("{}", serde_json::to_string_pretty(&res)?);
            }
            GovernanceCommands::Suspend {
                node_id,
                reason,
                ttl,
                key,
                rpc,
            } => {
                send_action(
                    &rpc,
                    "rope_suspendNode",
                    serde_json::json!({
                        "method": "rope_suspendNode",
                        "node_id": node_id,
                        "reason": reason,
                        "ttl_secs": ttl,
                    }),
                    key,
                )
                .await?
            }
            GovernanceCommands::Isolate {
                node_id,
                reason,
                key,
                rpc,
            } => {
                send_action(
                    &rpc,
                    "rope_isolateNode",
                    serde_json::json!({
                        "method": "rope_isolateNode",
                        "node_id": node_id,
                        "reason": reason,
                    }),
                    key,
                )
                .await?
            }
            GovernanceCommands::Erase {
                node_id,
                reason,
                key,
                rpc,
            } => {
                send_action(
                    &rpc,
                    "rope_eraseNode",
                    serde_json::json!({
                        "method": "rope_eraseNode",
                        "node_id": node_id,
                        "reason": reason,
                    }),
                    key,
                )
                .await?
            }
            GovernanceCommands::AnchorDeployer { force, rpc } => {
                let client = RpcClient::new(&rpc);
                let params = vec![serde_json::json!({ "force": force })];
                let res = client
                    .call("rope_anchorDeployerAttestation", params)
                    .await?;
                println!("{}", serde_json::to_string_pretty(&res)?);
            }
            GovernanceCommands::ListDeployerAttestations { wallet, rpc } => {
                let client = RpcClient::new(&rpc);
                let params = match wallet {
                    Some(w) => vec![serde_json::json!({ "wallet": w })],
                    None => vec![],
                };
                let res = client.call("rope_listDeployerAttestations", params).await?;
                println!("{}", serde_json::to_string_pretty(&res)?);
            }
        }
        Ok(())
    }

    async fn send_action(
        rpc: &str,
        method: &str,
        mut action: serde_json::Value,
        key: PathBuf,
    ) -> anyhow::Result<()> {
        let now = Utc::now().to_rfc3339();
        let nonce = Uuid::new_v4().to_string();
        if let Some(obj) = action.as_object_mut() {
            obj.insert(
                "issued_at".to_string(),
                serde_json::Value::String(now.clone()),
            );
            obj.insert(
                "nonce".to_string(),
                serde_json::Value::String(nonce.clone()),
            );
        }
        let canonical = canonical_json(&action);
        let key_bytes = std::fs::read(expand_path(&key))?;
        if key_bytes.len() < 32 {
            anyhow::bail!("signing key too short");
        }
        let seed: [u8; 32] = key_bytes[..32].try_into()?;
        let signing = SigningKey::from_bytes(&seed);
        let sig = signing.sign(canonical.as_bytes());
        let pubkey_hex = hex::encode(signing.verifying_key().to_bytes());
        let sig_hex = hex::encode(sig.to_bytes());

        // Build the RPC params: same fields, plus signature + pubkey
        let mut params_obj = action.as_object().cloned().unwrap_or_default();
        params_obj.remove("method"); // method goes outside
        params_obj.insert(
            "signature".to_string(),
            serde_json::Value::String(sig_hex.clone()),
        );
        params_obj.insert(
            "pubkey".to_string(),
            serde_json::Value::String(pubkey_hex.clone()),
        );

        let client = RpcClient::new(rpc);
        let res = client
            .call(method, vec![serde_json::Value::Object(params_obj)])
            .await?;
        println!("{}", serde_json::to_string_pretty(&res)?);
        eprintln!("\n  signed_by: {pubkey_hex}");
        eprintln!("  signature: {sig_hex}");
        eprintln!("  nonce:     {nonce}");
        Ok(())
    }
}

mod deploy {
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::sync::Arc;

    use rope_deployer::providers::digitalocean::DigitalOceanProvider;
    use rope_deployer::providers::exoscale::ExoscaleProvider;
    use rope_deployer::providers::local::LocalProvider;
    use rope_deployer::types::{NodeKind, Provider, ProvisionRequest};
    use rope_deployer::{AppState, ProviderRegistry};

    pub struct DeployArgs {
        pub provider: String,
        pub kind: String,
        pub region: Option<String>,
        pub size: String,
        pub identity: Option<PathBuf>,
        pub dry_run: bool,
    }

    fn map_provider(s: &str) -> anyhow::Result<Provider> {
        Ok(match s {
            "local" => Provider::Local,
            "exoscale" => Provider::Exoscale,
            "digitalocean" => Provider::Digitalocean,
            other => anyhow::bail!("Unknown provider: {other}"),
        })
    }

    fn map_kind(s: &str) -> anyhow::Result<NodeKind> {
        Ok(match s {
            "rpc-slot" => NodeKind::Rpc,
            "witness" => NodeKind::Witness,
            "community-node" | "databox" => NodeKind::Seeder,
            other => anyhow::bail!("Unknown node kind: {other}"),
        })
    }

    /// Read deployer DID + ONCHAINID from `--identity` (a Datawallet+
    /// claim toml/json) or fall back to `~/.rope/identity.toml`.
    /// For the MVP we accept a TOML file with two top-level keys:
    /// `did = "did:dwp:..."` and `onchainid = "0x..."`.
    fn load_identity(identity: Option<&PathBuf>) -> anyhow::Result<(String, String, String)> {
        let path = match identity {
            Some(p) => p.clone(),
            None => {
                let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
                home.join(".rope").join("identity.toml")
            }
        };

        if !path.exists() {
            // Foundation-self deploys can run without a DID claim — they
            // are bootstrapped from master-nodes.toml founder keys.
            return Ok((
                "did:dwp:foundation:bootstrap".to_string(),
                "0x0000000000000000000000000000000000000000".to_string(),
                "Datachain Foundation".to_string(),
            ));
        }

        let raw = std::fs::read_to_string(&path)?;
        let v: toml::Value = toml::from_str(&raw)?;
        let did = v
            .get("did")
            .and_then(|x| x.as_str())
            .unwrap_or("did:dwp:unknown")
            .to_string();
        let oid = v
            .get("onchainid")
            .and_then(|x| x.as_str())
            .unwrap_or("0x0")
            .to_string();
        let project = v
            .get("project_name")
            .and_then(|x| x.as_str())
            .unwrap_or("rope-node")
            .to_string();
        Ok((did, oid, project))
    }

    pub async fn run(args: DeployArgs) -> anyhow::Result<()> {
        let provider = map_provider(&args.provider)?;
        let kind = map_kind(&args.kind)?;
        let (did, onchainid, project_name) = load_identity(args.identity.as_ref())?;

        println!("=== rope deploy ===");
        println!();
        println!("  provider:   {}", provider.as_str());
        println!("  kind:       {}", kind.as_str());
        println!(
            "  region:     {}",
            args.region.as_deref().unwrap_or("<provider default>")
        );
        println!("  size:       {}", args.size);
        println!("  tenant_did: {}", did);
        println!("  onchainid:  {}", onchainid);
        println!("  project:    {}", project_name);
        println!("  dry_run:    {}", args.dry_run);
        println!();

        let registry = ProviderRegistry::new();
        registry.register(Arc::new(LocalProvider::new()));
        registry.register(Arc::new(ExoscaleProvider::from_env()));
        registry.register(Arc::new(DigitalOceanProvider::from_env()));
        let state = Arc::new(AppState::new(registry));

        let req = ProvisionRequest {
            tenant_did: did.clone(),
            tenant_onchainid: onchainid,
            project_name,
            provider,
            zone: args.region.unwrap_or_default(),
            instance_size: args.size,
            node_kind: kind,
            ssh_pubkey: std::env::var("ROPE_SSH_PUBKEY").unwrap_or_default(),
            labels: BTreeMap::new(),
        };

        if args.dry_run {
            println!("[dry-run] would POST /v1/instances with:");
            println!("{}", serde_json::to_string_pretty(&req)?);
            return Ok(());
        }

        let resp = rope_deployer::api::provision(state, req).await?;
        println!(
            "{}",
            serde_json::to_string_pretty(&resp)
                .unwrap_or_else(|_| "<failed to serialize response>".into())
        );
        if resp.dry_run {
            println!();
            println!("note: {}", resp.note);
            println!(
                "      to enable live provisioning, set EXOSCALE_API_KEY/SECRET \
                 (or DIGITALOCEAN_TOKEN) and re-run."
            );
        }
        Ok(())
    }
}
