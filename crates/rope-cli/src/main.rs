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
    }

    Ok(())
}
