//! Genesis generation for Datachain Rope
//!
//! Implements the DC FAT tokenomics:
//! - Genesis supply: 10 billion FAT
//! - Asymptotic maximum: ~18 billion FAT (halving model)
//! - Era 1 (2026-2029): 500M FAT/year distributed to validators

use serde::{Deserialize, Serialize};

/// Genesis configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Genesis {
    /// Chain ID
    pub chain_id: u64,
    /// Genesis timestamp (Unix seconds)
    pub timestamp: i64,
    /// Genesis hash (BLAKE3)
    pub genesis_hash: [u8; 32],
    /// Genesis string ID (first string in the lattice)
    pub genesis_string_id: [u8; 32],
    /// Initial validators
    pub validators: Vec<GenesisValidator>,
    /// Initial token distribution
    pub allocations: Vec<TokenAllocation>,
    /// Network parameters
    pub params: NetworkParams,
    /// Era configuration
    pub era_config: EraConfig,
}

/// Genesis validator
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenesisValidator {
    /// Validator node ID (BLAKE3 hash of public key)
    pub node_id: String,
    /// Validator libp2p peer ID
    pub peer_id: String,
    /// Validator public key (Ed25519 + Dilithium3 hybrid)
    pub pubkey: String,
    /// Validator name/label
    pub name: String,
    /// Initial stake (in FAT wei, 18 decimals)
    pub stake: String,
    /// Multiaddr for P2P connection
    pub multiaddr: String,
    /// Is this a foundation validator?
    pub foundation: bool,
}

/// Token allocation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenAllocation {
    /// Address (0x hex format)
    pub address: String,
    /// Amount in FAT wei (18 decimals)
    pub amount: String,
    /// Description/purpose
    pub label: String,
    /// Vesting schedule (optional)
    pub vesting: Option<VestingSchedule>,
}

/// Vesting schedule
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VestingSchedule {
    /// Start timestamp (Unix seconds)
    pub start: i64,
    /// Cliff duration (seconds)
    pub cliff: u64,
    /// Total vesting duration (seconds)
    pub duration: u64,
}

/// Network parameters
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkParams {
    /// String interval target (ms) - anchor string creation interval
    pub string_interval_ms: u64,
    /// Minimum testimonies for finality
    pub min_testimonies: u32,
    /// Maximum validators
    pub max_validators: u32,
    /// Minimum stake to become validator (FAT wei)
    pub min_stake: String,
    /// Genesis supply (FAT wei)
    pub genesis_supply: String,
    /// Maximum supply (asymptotic, FAT wei)  
    pub max_supply: String,
    /// AI testimony agents required per anchor
    pub ai_agents_required: u32,
    /// Epoch length (number of anchor strings)
    pub epoch_length: u64,
}

/// Era configuration (halving model)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EraConfig {
    /// Current era number
    pub current_era: u32,
    /// Era duration in years
    pub era_duration_years: u32,
    /// Base annual emission for Era 1 (FAT wei)
    pub era1_annual_emission: String,
    /// Halving factor per era
    pub halving_factor: f64,
}

/// DC FAT tokenomics constants
pub mod tokenomics {
    /// 1 FAT = 10^18 wei (same as ETH)
    pub const DECIMALS: u32 = 18;
    
    /// 1 FAT in wei
    pub const FAT: u128 = 1_000_000_000_000_000_000;
    
    /// Genesis supply: 10 billion FAT
    pub const GENESIS_SUPPLY: u128 = 10_000_000_000 * FAT;
    
    /// Maximum supply: ~18 billion FAT (asymptotic)
    pub const MAX_SUPPLY: u128 = 18_000_000_000 * FAT;
    
    /// Era 1 annual emission: 500 million FAT
    pub const ERA1_ANNUAL_EMISSION: u128 = 500_000_000 * FAT;
    
    /// Minimum stake for validator: 1 million FAT
    pub const MIN_VALIDATOR_STAKE: u128 = 1_000_000 * FAT;
}

/// Generate genesis for testnet
pub fn generate_testnet_genesis() -> anyhow::Result<Genesis> {
    let timestamp = chrono::Utc::now().timestamp();
    
    let validators = vec![
        GenesisValidator {
            node_id: "6fd19624df05cb790d17903575013344b7f6432aa6a27473da157c0904585c15".to_string(),
            peer_id: "12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM".to_string(),
            pubkey: "".to_string(),
            name: "Datachain Foundation Boot1".to_string(),
            stake: tokenomics::MIN_VALIDATOR_STAKE.to_string(),
            multiaddr: "/ip4/92.243.26.189/tcp/9000/p2p/12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM".to_string(),
            foundation: true,
        },
    ];
    
    let allocations = vec![
        TokenAllocation {
            address: "0x0000000000000000000000000000000000000001".to_string(),
            amount: (5_000_000_000u128 * tokenomics::FAT).to_string(),
            label: "Testnet Faucet".to_string(),
            vesting: None,
        },
        TokenAllocation {
            address: "0x0000000000000000000000000000000000000002".to_string(),
            amount: (5_000_000_000u128 * tokenomics::FAT).to_string(),
            label: "Testnet Validator Rewards".to_string(),
            vesting: None,
        },
    ];
    
    let params = NetworkParams {
        string_interval_ms: 4200,
        min_testimonies: 1,
        max_validators: 127,
        min_stake: tokenomics::MIN_VALIDATOR_STAKE.to_string(),
        genesis_supply: tokenomics::GENESIS_SUPPLY.to_string(),
        max_supply: tokenomics::MAX_SUPPLY.to_string(),
        ai_agents_required: 1,
        epoch_length: 21600,
    };
    
    let era_config = EraConfig {
        current_era: 1,
        era_duration_years: 4,
        era1_annual_emission: tokenomics::ERA1_ANNUAL_EMISSION.to_string(),
        halving_factor: 0.5,
    };
    
    // Compute genesis hash
    let mut hash_input = Vec::new();
    hash_input.extend_from_slice(&271829u64.to_le_bytes());
    hash_input.extend_from_slice(&timestamp.to_le_bytes());
    hash_input.extend_from_slice(b"DATACHAIN_ROPE_GENESIS_V1");
    let genesis_hash = *blake3::hash(&hash_input).as_bytes();
    
    let mut string_hash_input = genesis_hash.to_vec();
    string_hash_input.extend_from_slice(b"GENESIS_STRING");
    let genesis_string_id = *blake3::hash(&string_hash_input).as_bytes();
    
    Ok(Genesis {
        chain_id: 271829,
        timestamp,
        genesis_hash,
        genesis_string_id,
        validators,
        allocations,
        params,
        era_config,
    })
}

/// Legacy function for CLI compatibility
pub fn generate_genesis(validators: u32, chain_id: u64) -> anyhow::Result<Genesis> {
    let timestamp = chrono::Utc::now().timestamp();
    
    let mut validator_entries = Vec::new();
    for i in 0..validators {
        validator_entries.push(GenesisValidator {
            node_id: format!("{:064x}", i),
            peer_id: format!("12D3KooW{:040x}", i),
            pubkey: format!("0x{}", hex::encode(&[i as u8; 32])),
            name: format!("Validator {}", i + 1),
            stake: tokenomics::MIN_VALIDATOR_STAKE.to_string(),
            multiaddr: format!("/ip4/127.0.0.1/tcp/{}/p2p/12D3KooW{:040x}", 9000 + i, i),
            foundation: i == 0,
        });
    }
    
    let is_testnet = chain_id == 271829;
    
    let allocations = vec![
        TokenAllocation {
            address: "0x0000000000000000000000000000000000000001".to_string(),
            amount: tokenomics::GENESIS_SUPPLY.to_string(),
            label: "Genesis Allocation".to_string(),
            vesting: None,
        },
    ];
    
    let params = NetworkParams {
        string_interval_ms: 4200,
        min_testimonies: if is_testnet { 1 } else { 5 },
        max_validators: 127,
        min_stake: tokenomics::MIN_VALIDATOR_STAKE.to_string(),
        genesis_supply: tokenomics::GENESIS_SUPPLY.to_string(),
        max_supply: tokenomics::MAX_SUPPLY.to_string(),
        ai_agents_required: if is_testnet { 1 } else { 3 },
        epoch_length: 21600,
    };
    
    let era_config = EraConfig {
        current_era: 1,
        era_duration_years: 4,
        era1_annual_emission: tokenomics::ERA1_ANNUAL_EMISSION.to_string(),
        halving_factor: 0.5,
    };
    
    let mut hash_input = Vec::new();
    hash_input.extend_from_slice(&chain_id.to_le_bytes());
    hash_input.extend_from_slice(&timestamp.to_le_bytes());
    hash_input.extend_from_slice(b"DATACHAIN_ROPE_GENESIS_V1");
    let genesis_hash = *blake3::hash(&hash_input).as_bytes();
    
    let mut string_hash_input = genesis_hash.to_vec();
    string_hash_input.extend_from_slice(b"GENESIS_STRING");
    let genesis_string_id = *blake3::hash(&string_hash_input).as_bytes();
    
    Ok(Genesis {
        chain_id,
        timestamp,
        genesis_hash,
        genesis_string_id,
        validators: validator_entries,
        allocations,
        params,
        era_config,
    })
}
