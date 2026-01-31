//! # Datachain Rope Network Configuration
//! 
//! Defines network parameters for wallet and tool integration.
//! Compatible with EVM-style JSON-RPC interfaces for MetaMask, etc.
//! 
//! ## Network Information
//! 
//! | Parameter | Value |
//! |-----------|-------|
//! | Network Name | Datachain Rope |
//! | Chain ID | 271828 (0x425D4) |
//! | Currency Symbol | FAT |
//! | Currency Name | DC FAT |
//! | RPC URL | https://erpc.datachain.network |
//! | Block Explorer | https://dcscan.io |
//! 
//! ## Chain ID Selection Rationale
//! 
//! 271828 was chosen because:
//! - Represents Euler's number e ≈ 2.71828 - symbolizing exponential growth
//! - Not conflicting with any existing EVM chain IDs
//! - Memorable and mathematically meaningful
//! - Within the safe range for wallet compatibility (< 2^31)

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// CHAIN CONSTANTS
// ============================================================================

/// Primary Chain ID for Datachain Rope Smartchain Mainnet
/// 271828 = Euler's number e ≈ 2.71828 (exponential growth symbolism)
pub const CHAIN_ID_MAINNET: u64 = 271828;

/// Testnet Chain ID
/// 271829 = Mainnet + 1
pub const CHAIN_ID_TESTNET: u64 = 271829;

/// Development/Local Chain ID
pub const CHAIN_ID_DEVNET: u64 = 271830;

/// Network Name
pub const NETWORK_NAME: &str = "Datachain Rope";

/// Network Name (Short)
pub const NETWORK_NAME_SHORT: &str = "Datachain";

/// Currency Symbol
pub const CURRENCY_SYMBOL: &str = "FAT";

/// Currency Name
pub const CURRENCY_NAME: &str = "DC FAT";

/// Currency Full Name (same as Currency Name for this network)
pub const CURRENCY_FULL_NAME: &str = "DC FAT";

/// Currency Decimals (like ETH - 18 decimals)
pub const CURRENCY_DECIMALS: u8 = 18;

// ============================================================================
// DOMAIN CONFIGURATION
// ============================================================================

/// Primary domain
pub const DOMAIN_PRIMARY: &str = "datachain.network";

/// Secondary domain
pub const DOMAIN_SECONDARY: &str = "rope.network";

// ============================================================================
// RPC ENDPOINTS
// ============================================================================

/// Primary RPC endpoint
pub const RPC_URL_PRIMARY: &str = "https://erpc.datachain.network";

/// Secondary RPC endpoint
pub const RPC_URL_SECONDARY: &str = "https://erpc.rope.network";

/// WebSocket endpoint
pub const WS_URL_PRIMARY: &str = "wss://ws.datachain.network";

/// WebSocket secondary
pub const WS_URL_SECONDARY: &str = "wss://ws.rope.network";

// ============================================================================
// EXPLORER & SERVICES
// ============================================================================

/// Block Explorer URL
pub const BLOCK_EXPLORER_URL: &str = "https://dcscan.io";

/// API endpoint for the explorer
pub const EXPLORER_API_URL: &str = "https://api.dcscan.io";

/// Faucet URL (testnet)
pub const FAUCET_URL: &str = "https://faucet.datachain.network";

/// Bridge URL
pub const BRIDGE_URL: &str = "https://bridge.datachain.network";

// ============================================================================
// NETWORK CONFIGURATION STRUCT
// ============================================================================

/// Complete network configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// Chain ID
    pub chain_id: u64,
    
    /// Network name
    pub network_name: String,
    
    /// Network name short
    pub network_name_short: String,
    
    /// Currency symbol
    pub currency_symbol: String,
    
    /// Currency name
    pub currency_name: String,
    
    /// Currency decimals
    pub currency_decimals: u8,
    
    /// RPC URLs
    pub rpc_urls: Vec<String>,
    
    /// WebSocket URLs
    pub ws_urls: Vec<String>,
    
    /// Block explorer URL
    pub block_explorer_url: String,
    
    /// Is testnet
    pub is_testnet: bool,
    
    /// Genesis hash
    pub genesis_hash: [u8; 32],
    
    /// Genesis timestamp
    pub genesis_timestamp: i64,
}

impl NetworkConfig {
    /// Create mainnet configuration
    pub fn mainnet() -> Self {
        Self {
            chain_id: CHAIN_ID_MAINNET,
            network_name: NETWORK_NAME.to_string(),
            network_name_short: NETWORK_NAME_SHORT.to_string(),
            currency_symbol: CURRENCY_SYMBOL.to_string(),
            currency_name: CURRENCY_NAME.to_string(),
            currency_decimals: CURRENCY_DECIMALS,
            rpc_urls: vec![RPC_URL_PRIMARY.to_string(), RPC_URL_SECONDARY.to_string()],
            ws_urls: vec![WS_URL_PRIMARY.to_string(), WS_URL_SECONDARY.to_string()],
            block_explorer_url: BLOCK_EXPLORER_URL.to_string(),
            is_testnet: false,
            genesis_hash: Self::mainnet_genesis_hash(),
            genesis_timestamp: Self::mainnet_genesis_timestamp(),
        }
    }
    
    /// Create testnet configuration
    pub fn testnet() -> Self {
        Self {
            chain_id: CHAIN_ID_TESTNET,
            network_name: format!("{} Testnet", NETWORK_NAME),
            network_name_short: "Datachain Testnet".to_string(),
            currency_symbol: "DCR FAT".to_string(),
            currency_name: "DCR FAT".to_string(),
            currency_decimals: CURRENCY_DECIMALS,
            rpc_urls: vec![
                "https://testnet.erpc.datachain.network".to_string(),
                "https://testnet.erpc.rope.network".to_string(),
            ],
            ws_urls: vec!["wss://testnet.ws.datachain.network".to_string()],
            block_explorer_url: "https://testnet.dcscan.io".to_string(),
            is_testnet: true,
            genesis_hash: Self::testnet_genesis_hash(),
            genesis_timestamp: 0, // To be set at testnet launch
        }
    }
    
    /// Create devnet configuration
    pub fn devnet() -> Self {
        Self {
            chain_id: CHAIN_ID_DEVNET,
            network_name: format!("{} Devnet", NETWORK_NAME),
            network_name_short: "Devnet".to_string(),
            currency_symbol: format!("d{}", CURRENCY_SYMBOL),
            currency_name: format!("Dev {}", CURRENCY_NAME),
            currency_decimals: CURRENCY_DECIMALS,
            rpc_urls: vec!["http://localhost:8545".to_string()],
            ws_urls: vec!["ws://localhost:8546".to_string()],
            block_explorer_url: "http://localhost:4000".to_string(),
            is_testnet: true,
            genesis_hash: [0u8; 32],
            genesis_timestamp: 0,
        }
    }
    
    /// Mainnet genesis hash (to be updated at mainnet launch)
    fn mainnet_genesis_hash() -> [u8; 32] {
        // Hash of "Datachain Rope - Smartchain Genesis - Powering The Future of Internet"
        *blake3::hash(b"Datachain Rope - Smartchain Genesis - Powering The Future of Internet")
            .as_bytes()
    }
    
    /// Mainnet genesis timestamp (placeholder - TBD at launch)
    fn mainnet_genesis_timestamp() -> i64 {
        // Placeholder: January 1, 2026 00:00:00 UTC
        1767225600
    }
    
    /// Testnet genesis hash
    fn testnet_genesis_hash() -> [u8; 32] {
        *blake3::hash(b"Datachain Rope Smartchain Testnet Genesis").as_bytes()
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self::mainnet()
    }
}

// ============================================================================
// WALLET/METAMASK COMPATIBLE CONFIGURATION
// ============================================================================

/// MetaMask-compatible chain configuration
/// Can be used with wallet_addEthereumChain RPC method
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WalletChainConfig {
    /// Chain ID in hex format (e.g., "0x4CB2F")
    pub chain_id: String,
    
    /// Chain name
    pub chain_name: String,
    
    /// Native currency info
    pub native_currency: NativeCurrency,
    
    /// RPC URLs
    pub rpc_urls: Vec<String>,
    
    /// Block explorer URLs
    pub block_explorer_urls: Vec<String>,
    
    /// Icon URLs (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_urls: Option<Vec<String>>,
}

/// Native currency configuration for wallets
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NativeCurrency {
    /// Currency name
    pub name: String,
    
    /// Currency symbol
    pub symbol: String,
    
    /// Decimals
    pub decimals: u8,
}

impl WalletChainConfig {
    /// Create mainnet wallet configuration
    pub fn mainnet() -> Self {
        Self {
            chain_id: format!("0x{:X}", CHAIN_ID_MAINNET), // "0x4CB2F"
            chain_name: NETWORK_NAME.to_string(),
            native_currency: NativeCurrency {
                name: CURRENCY_FULL_NAME.to_string(),
                symbol: CURRENCY_SYMBOL.to_string(),
                decimals: CURRENCY_DECIMALS,
            },
            rpc_urls: vec![RPC_URL_PRIMARY.to_string(), RPC_URL_SECONDARY.to_string()],
            block_explorer_urls: vec![BLOCK_EXPLORER_URL.to_string()],
            icon_urls: Some(vec![
                "https://datachain.network/assets/icons/dc-fat-logo.svg".to_string(),
                "https://datachain.network/assets/icons/dc-fat-logo-128.png".to_string(),
            ]),
        }
    }
    
    /// Create testnet wallet configuration
    pub fn testnet() -> Self {
        Self {
            chain_id: format!("0x{:X}", CHAIN_ID_TESTNET),
            chain_name: format!("{} Testnet", NETWORK_NAME),
            native_currency: NativeCurrency {
                name: "DCR FAT".to_string(),
                symbol: "DCR FAT".to_string(),
                decimals: CURRENCY_DECIMALS,
            },
            rpc_urls: vec!["https://testnet.erpc.datachain.network".to_string()],
            block_explorer_urls: vec!["https://testnet.dcscan.io".to_string()],
            icon_urls: None,
        }
    }
    
    /// Serialize to JSON for wallet_addEthereumChain
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

// ============================================================================
// GENESIS CONFIGURATION
// ============================================================================

/// Genesis configuration for the network
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenesisConfig {
    /// Chain configuration
    pub config: ChainParams,
    
    /// Initial allocations (address -> balance)
    pub alloc: HashMap<String, GenesisAllocation>,
    
    /// Coinbase address
    pub coinbase: String,
    
    /// Difficulty (for PoW compatibility - set to 1 for PoS/Testimony)
    pub difficulty: String,
    
    /// Extra data
    pub extra_data: String,
    
    /// Gas limit
    pub gas_limit: String,
    
    /// Nonce
    pub nonce: String,
    
    /// Mixhash (for compatibility)
    pub mix_hash: String,
    
    /// Parent hash (zero for genesis)
    pub parent_hash: String,
    
    /// Timestamp
    pub timestamp: String,
}

/// Chain parameters
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainParams {
    /// Chain ID
    pub chain_id: u64,
    
    /// Homestead block
    pub homestead_block: u64,
    
    /// EIP-150 block
    pub eip150_block: u64,
    
    /// EIP-155 block
    pub eip155_block: u64,
    
    /// EIP-158 block
    pub eip158_block: u64,
    
    /// Byzantium block
    pub byzantium_block: u64,
    
    /// Constantinople block
    pub constantinople_block: u64,
    
    /// Petersburg block
    pub petersburg_block: u64,
    
    /// Istanbul block
    pub istanbul_block: u64,
    
    /// Berlin block
    pub berlin_block: u64,
    
    /// London block
    pub london_block: u64,
    
    /// Testimony consensus config (our custom consensus)
    pub testimony: TestimonyConfig,
}

/// Testimony consensus configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestimonyConfig {
    /// String production interval (milliseconds)
    pub string_interval_ms: u64,
    
    /// Epoch length (number of strings)
    pub epoch_length: u64,
    
    /// Minimum validators
    pub min_validators: u32,
    
    /// Maximum validators
    pub max_validators: u32,
    
    /// Minimum stake for validator
    pub min_stake: String,
    
    /// AI testimony agents required
    pub ai_agents_required: u32,
}

/// Genesis allocation entry
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenesisAllocation {
    /// Balance in wei
    pub balance: String,
    
    /// Optional: code
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    
    /// Optional: storage
    #[serde(skip_serializing_if = "Option::is_none")]
    pub storage: Option<HashMap<String, String>>,
}

impl GenesisConfig {
    /// Create mainnet genesis
    pub fn mainnet() -> Self {
        let mut alloc = HashMap::new();
        
        // Datachain Foundation allocation (40%)
        alloc.insert(
            "0x0000000000000000000000000000000000000001".to_string(),
            GenesisAllocation {
                balance: "4000000000000000000000000000".to_string(), // 4B FAT
                code: None,
                storage: None,
            },
        );
        
        // Ecosystem Development (25%)
        alloc.insert(
            "0x0000000000000000000000000000000000000002".to_string(),
            GenesisAllocation {
                balance: "2500000000000000000000000000".to_string(), // 2.5B FAT
                code: None,
                storage: None,
            },
        );
        
        // Community & Incentives (20%)
        alloc.insert(
            "0x0000000000000000000000000000000000000003".to_string(),
            GenesisAllocation {
                balance: "2000000000000000000000000000".to_string(), // 2B FAT
                code: None,
                storage: None,
            },
        );
        
        // Team & Advisors (10%)
        alloc.insert(
            "0x0000000000000000000000000000000000000004".to_string(),
            GenesisAllocation {
                balance: "1000000000000000000000000000".to_string(), // 1B FAT
                code: None,
                storage: None,
            },
        );
        
        // Initial Liquidity (5%)
        alloc.insert(
            "0x0000000000000000000000000000000000000005".to_string(),
            GenesisAllocation {
                balance: "500000000000000000000000000".to_string(), // 0.5B FAT
                code: None,
                storage: None,
            },
        );
        
        Self {
            config: ChainParams {
                chain_id: CHAIN_ID_MAINNET,
                homestead_block: 0,
                eip150_block: 0,
                eip155_block: 0,
                eip158_block: 0,
                byzantium_block: 0,
                constantinople_block: 0,
                petersburg_block: 0,
                istanbul_block: 0,
                berlin_block: 0,
                london_block: 0,
                testimony: TestimonyConfig {
                    string_interval_ms: 1000, // 1 second per string
                    epoch_length: 43200,      // ~12 hours
                    min_validators: 21,
                    max_validators: 100,
                    min_stake: "1000000000000000000000".to_string(), // 1000 FAT
                    ai_agents_required: 5,
                },
            },
            alloc,
            coinbase: "0x0000000000000000000000000000000000000000".to_string(),
            difficulty: "0x1".to_string(),
            extra_data: format!(
                "0x{}",
                hex::encode(
                    "Datachain Rope - Smartchain Genesis - Powering The Future of Internet"
                )
            ),
            gas_limit: "0x2fefd8".to_string(), // 30M gas
            nonce: "0x0000000000000000".to_string(),
            mix_hash: "0x0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            parent_hash: "0x0000000000000000000000000000000000000000000000000000000000000000"
                .to_string(),
            timestamp: format!("0x{:X}", NetworkConfig::mainnet_genesis_timestamp()),
        }
    }
    
    /// Serialize to JSON
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }
}

// ============================================================================
// NETWORK ENDPOINTS
// ============================================================================

/// Network service endpoints
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkEndpoints {
    /// JSON-RPC endpoints
    pub rpc: RpcEndpoints,
    
    /// WebSocket endpoints
    pub websocket: WsEndpoints,
    
    /// GraphQL endpoint
    pub graphql: Option<String>,
    
    /// IPFS gateway
    pub ipfs: Option<String>,
    
    /// Bridge endpoints
    pub bridges: BridgeEndpoints,
}

/// RPC endpoints
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcEndpoints {
    /// Primary RPC
    pub primary: String,
    
    /// Secondary RPC
    pub secondary: String,
    
    /// Archive node RPC
    pub archive: Option<String>,
    
    /// Debug RPC (admin only)
    pub debug: Option<String>,
}

/// WebSocket endpoints
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WsEndpoints {
    /// Primary WebSocket
    pub primary: String,
    
    /// Secondary WebSocket
    pub secondary: Option<String>,
}

/// Bridge endpoints
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeEndpoints {
    /// Ethereum bridge
    pub ethereum: Option<BridgeConfig>,
    
    /// XDC bridge
    pub xdc: Option<BridgeConfig>,
    
    /// Polkadot bridge
    pub polkadot: Option<BridgeConfig>,
}

/// Bridge configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BridgeConfig {
    /// Endpoint URL
    pub endpoint: String,
    
    /// Contract address (if applicable)
    pub contract_address: Option<String>,
    
    /// Is enabled
    pub enabled: bool,
}

impl NetworkEndpoints {
    /// Mainnet endpoints
    pub fn mainnet() -> Self {
        Self {
            rpc: RpcEndpoints {
                primary: RPC_URL_PRIMARY.to_string(),
                secondary: RPC_URL_SECONDARY.to_string(),
                archive: Some("https://archive.erpc.datachain.network".to_string()),
                debug: None,
            },
            websocket: WsEndpoints {
                primary: WS_URL_PRIMARY.to_string(),
                secondary: Some(WS_URL_SECONDARY.to_string()),
            },
            graphql: Some("https://graph.datachain.network/subgraphs".to_string()),
            ipfs: Some("https://ipfs.datachain.network".to_string()),
            bridges: BridgeEndpoints {
                ethereum: Some(BridgeConfig {
                    endpoint: "https://bridge.datachain.network/ethereum".to_string(),
                    contract_address: Some(
                        "0x0b44547be0a0df5dcd5327de8ea73680517c5a54".to_string(),
                    ),
                    enabled: true,
                }),
                xdc: Some(BridgeConfig {
                    endpoint: "https://bridge.datachain.network/xdc".to_string(),
                    contract_address: Some(
                        "0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a".to_string(),
                    ),
                    enabled: true,
                }),
                polkadot: Some(BridgeConfig {
                    endpoint: "https://bridge.datachain.network/polkadot".to_string(),
                    contract_address: None,
                    enabled: true,
                }),
            },
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
    fn test_chain_id() {
        assert_eq!(CHAIN_ID_MAINNET, 314159);
        assert_eq!(CHAIN_ID_TESTNET, 314160);
        
        // Chain ID should be unique and not conflict with known chains
        // See: https://chainlist.org/
        assert!(CHAIN_ID_MAINNET > 100000); // Avoid common chain IDs
    }
    
    #[test]
    fn test_network_config() {
        let config = NetworkConfig::mainnet();
        
        assert_eq!(config.chain_id, CHAIN_ID_MAINNET);
        assert_eq!(config.currency_symbol, "FAT");
        assert_eq!(config.currency_decimals, 18);
        assert!(!config.is_testnet);
    }
    
    #[test]
    fn test_wallet_config() {
        let config = WalletChainConfig::mainnet();
        
        assert_eq!(config.chain_id, "0x425D4"); // 271828 in hex
        assert_eq!(config.native_currency.symbol, "FAT");
        assert_eq!(config.native_currency.decimals, 18);
        
        // Test JSON serialization
        let json = config.to_json();
        assert!(json.contains("chainId"));
        assert!(json.contains("FAT"));
    }
    
    #[test]
    fn test_genesis_config() {
        let genesis = GenesisConfig::mainnet();
        
        assert_eq!(genesis.config.chain_id, CHAIN_ID_MAINNET);
        assert_eq!(genesis.config.testimony.ai_agents_required, 5);
        
        // Check allocations sum to 10B
        let total: u128 = genesis
            .alloc
            .values()
            .map(|a| a.balance.parse::<u128>().unwrap_or(0))
            .sum();
        
        // 10B FAT in wei (10^9 * 10^18 = 10^27)
        assert_eq!(total, 10_000_000_000_000_000_000_000_000_000u128);
    }
    
    #[test]
    fn test_network_endpoints() {
        let endpoints = NetworkEndpoints::mainnet();
        
        assert!(endpoints.rpc.primary.contains("datachain.network"));
        assert!(endpoints.websocket.primary.starts_with("wss://"));
    }
}
