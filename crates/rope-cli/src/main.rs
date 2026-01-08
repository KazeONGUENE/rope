//! Datachain Rope CLI
//! 
//! Command-line interface for running Rope nodes.

use clap::{Parser, Subcommand};
use rope_node::{RopeNode, NodeConfig};
use rope_crypto::keys::KeyPair;
use std::path::PathBuf;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(Parser)]
#[command(name = "rope")]
#[command(author = "Datachain Foundation")]
#[command(version = "0.1.0")]
#[command(about = "Datachain Rope - Distributed Information Communication Protocol", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    
    /// Verbose logging
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a Rope node
    Node {
        /// Configuration file path
        #[arg(short, long, default_value = "rope.toml")]
        config: PathBuf,
        
        /// Data directory
        #[arg(short, long, default_value = "~/.rope")]
        data_dir: PathBuf,
        
        /// Node mode: validator, relay, or seeder
        #[arg(short, long, default_value = "relay")]
        mode: String,
        
        /// Network: mainnet or testnet
        #[arg(short, long, default_value = "mainnet")]
        network: String,
    },
    
    /// Generate a new keypair
    Keygen {
        /// Output directory for keys
        #[arg(short, long, default_value = "~/.rope/keys")]
        output: PathBuf,
        
        /// Generate quantum-resistant keys
        #[arg(long)]
        quantum: bool,
    },
    
    /// Show node information
    Info {
        /// Data directory
        #[arg(short, long, default_value = "~/.rope")]
        data_dir: PathBuf,
    },
    
    /// Initialize a new genesis federation
    Genesis {
        /// Number of initial validators
        #[arg(short, long, default_value = "21")]
        validators: u32,
        
        /// Chain ID
        #[arg(long, default_value = "314159")]
        chain_id: u64,
        
        /// Output file for genesis
        #[arg(short, long, default_value = "genesis.json")]
        output: PathBuf,
    },
    
    /// Query the network
    Query {
        #[command(subcommand)]
        query: QueryCommands,
    },
    
    /// Token operations
    Token {
        #[command(subcommand)]
        token: TokenCommands,
    },
    
    /// Version information
    Version,
}

#[derive(Subcommand)]
enum QueryCommands {
    /// Get string by ID
    String {
        /// String ID (hex)
        id: String,
    },
    /// Get network status
    Status,
    /// List connected peers
    Peers,
    /// Get validator set
    Validators,
}

#[derive(Subcommand)]
enum TokenCommands {
    /// Check balance
    Balance {
        /// Address (hex)
        address: String,
    },
    /// Transfer tokens
    Transfer {
        /// Recipient address
        to: String,
        /// Amount
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
        .with(tracing_subscriber::fmt::layer()
            .with_target(true)
            .with_thread_ids(false)
            .with_file(false))
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
        Commands::Node { config, data_dir, mode, network } => {
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
            let node_config = if config_path.exists() {
                let content = std::fs::read_to_string(&config_path)?;
                toml::from_str(&content)?
            } else {
                tracing::info!("Config not found, using defaults for {}", network);
                NodeConfig::for_network(&network)?
            };
            
            // Create data directory
            std::fs::create_dir_all(&data_dir)?;
            
            // Start node
            let node = RopeNode::new(node_config, data_dir).await?;
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
            println!("  Mainnet Chain ID: 314159");
            println!("  Testnet Chain ID: 314160");
            println!("  RPC: https://erpc.datachain.network");
            println!("  Explorer: https://dcscan.io");
            println!("");
            println!("https://datachain.network");
        }
        
        Commands::Genesis { validators, chain_id, output } => {
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
            match query {
                QueryCommands::String { id } => {
                    println!("Querying string: {}", id);
                    // TODO: Implement RPC query
                    println!("RPC client not yet implemented");
                }
                QueryCommands::Status => {
                    println!("Network Status:");
                    // TODO: Implement status query
                    println!("RPC client not yet implemented");
                }
                QueryCommands::Peers => {
                    println!("Connected Peers:");
                    // TODO: Implement peers query
                    println!("RPC client not yet implemented");
                }
                QueryCommands::Validators => {
                    println!("Validator Set:");
                    // TODO: Implement validators query
                    println!("RPC client not yet implemented");
                }
            }
        }
        
        Commands::Token { token } => {
            match token {
                TokenCommands::Balance { address } => {
                    println!("Balance for {}: ", address);
                    // TODO: Implement balance query
                    println!("RPC client not yet implemented");
                }
                TokenCommands::Transfer { to, amount } => {
                    println!("Transfer {} FAT to {}", amount, to);
                    // TODO: Implement transfer
                    println!("RPC client not yet implemented");
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
    }
    
    Ok(())
}
