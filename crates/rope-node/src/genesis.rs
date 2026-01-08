//! Genesis generation

use serde::{Deserialize, Serialize};

/// Genesis configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Genesis {
    /// Chain ID
    pub chain_id: u64,
    /// Genesis timestamp
    pub timestamp: i64,
    /// Genesis hash
    pub genesis_hash: [u8; 32],
    /// Initial validators
    pub validators: Vec<GenesisValidator>,
    /// Initial token distribution
    pub allocations: Vec<TokenAllocation>,
    /// Network parameters
    pub params: NetworkParams,
}

/// Genesis validator
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GenesisValidator {
    /// Validator public key
    pub pubkey: String,
    /// Validator name
    pub name: String,
    /// Initial stake
    pub stake: u64,
}

/// Token allocation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TokenAllocation {
    /// Address (hex)
    pub address: String,
    /// Amount
    pub amount: u64,
    /// Vesting schedule (optional)
    pub vesting: Option<VestingSchedule>,
}

/// Vesting schedule
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VestingSchedule {
    /// Start timestamp
    pub start: i64,
    /// Cliff duration (seconds)
    pub cliff: u64,
    /// Total duration (seconds)
    pub duration: u64,
}

/// Network parameters
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NetworkParams {
    /// Block time target (ms)
    pub block_time_ms: u64,
    /// Minimum testimonies for finality
    pub min_testimonies: u32,
    /// Maximum validators
    pub max_validators: u32,
    /// Minimum stake
    pub min_stake: u64,
    /// Genesis supply
    pub genesis_supply: u64,
    /// Annual inflation cap (percentage)
    pub annual_inflation_cap: f64,
}

/// Generate genesis
pub fn generate_genesis(validators: u32, chain_id: u64) -> anyhow::Result<Genesis> {
    let timestamp = chrono::Utc::now().timestamp();
    
    // Generate validator entries
    let mut validator_entries = Vec::new();
    for i in 0..validators {
        validator_entries.push(GenesisValidator {
            pubkey: format!("0x{}", hex::encode(&[i as u8; 32])),
            name: format!("Validator {}", i + 1),
            stake: 1_000_000, // 1M FAT minimum stake
        });
    }
    
    // Initial allocations
    let allocations = vec![
        TokenAllocation {
            address: "0x000000000000000000000000000000000000dead".to_string(),
            amount: 10_000_000_000, // 10B genesis supply
            vesting: None,
        },
    ];
    
    // Network params
    let params = NetworkParams {
        block_time_ms: 3000,
        min_testimonies: 5,
        max_validators: 127,
        min_stake: 1_000_000,
        genesis_supply: 10_000_000_000,
        annual_inflation_cap: 3.0,
    };
    
    // Compute genesis hash
    let mut hash_input = Vec::new();
    hash_input.extend_from_slice(&chain_id.to_le_bytes());
    hash_input.extend_from_slice(&timestamp.to_le_bytes());
    let genesis_hash = *blake3::hash(&hash_input).as_bytes();
    
    Ok(Genesis {
        chain_id,
        timestamp,
        genesis_hash,
        validators: validator_entries,
        allocations,
        params,
    })
}

