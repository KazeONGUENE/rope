//! # Rope Economics - DC FAT Tokenomics & Reward System
//!
//! Comprehensive economic model for Datachain Rope, inspired by Bitcoin and Ethereum.
//!
//! ## Key Features
//!
//! - **Bitcoin-style halving**: Emission reduces by 50% every 4 years
//! - **Ethereum-style APY**: Target ~5% yield at equilibrium
//! - **Performance-based rewards**: Multipliers for uptime, speed, green energy
//! - **Federation/Community rewards**: Activity-based tier system
//!
//! ## DC FAT Tokenomics
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────────┐
//! │                     DC FAT SUPPLY PROJECTION                            │
//! ├─────────────────────────────────────────────────────────────────────────┤
//! │  Genesis (2026):     10,000,000,000 FAT (10 billion)                    │
//! │  Year 5 (2030):      12,000,000,000 FAT                                 │
//! │  Year 10 (2035):     13,500,000,000 FAT                                 │
//! │  Year 20 (2045):     14,500,000,000 FAT                                 │
//! │  Asymptotic Max:     ~18,000,000,000 FAT                                │
//! └─────────────────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Emission Schedule
//!
//! | Era | Years | Annual Emission | Block Reward |
//! |-----|-------|-----------------|--------------|
//! | 1 | 2026-2029 | 500M FAT | 66.6 FAT |
//! | 2 | 2030-2033 | 250M FAT | 33.3 FAT |
//! | 3 | 2034-2037 | 125M FAT | 16.65 FAT |
//! | 4 | 2038-2041 | 62.5M FAT | 8.33 FAT |
//! | ... | ... | (halving continues) | ... |

pub mod emission;
pub mod rewards;
pub mod staking;
pub mod federation;
pub mod performance;
pub mod green_energy;
pub mod slashing;

// Re-exports
pub use emission::{EmissionSchedule, EmissionEra, AnchorReward};
pub use rewards::{RewardCalculator, ValidatorReward, NodeReward};
pub use staking::{StakeManager, ValidatorStake, StakeRequirements};
pub use federation::{FederationRewards, ActivityTier, CommunityRewards};
pub use performance::{PerformanceScore, PerformanceMetrics, PerformanceMultiplier};
pub use green_energy::{GreenEnergyVerification, EnergySource, GreenEnergyMultiplier};
pub use slashing::{SlashingEngine, SlashingOffense, SlashingPenalty};

/// DC FAT token constants
pub mod constants {
    /// Token symbol
    pub const SYMBOL: &str = "FAT";
    
    /// Token name
    pub const NAME: &str = "DATACHAIN Future Access Token";
    
    /// Decimal places (same as ETH)
    pub const DECIMALS: u8 = 18;
    
    /// One FAT in smallest unit (like wei for ETH)
    pub const ONE_FAT: u128 = 1_000_000_000_000_000_000; // 10^18
    
    /// Genesis supply: 10 billion FAT
    pub const GENESIS_SUPPLY: u128 = 10_000_000_000 * ONE_FAT;
    
    /// Era 1 annual emission: 500 million FAT
    pub const ANNUAL_EMISSION_ERA1: u128 = 500_000_000 * ONE_FAT;
    
    /// Halving interval: 4 years in seconds
    pub const HALVING_INTERVAL_SECS: u64 = 4 * 365 * 24 * 3600; // ~126,144,000 seconds
    
    /// Anchor interval: ~4.2 seconds
    pub const ANCHOR_INTERVAL_SECS: f64 = 4.2;
    
    /// Anchors per year: ~7,500,000
    pub const ANCHORS_PER_YEAR: u64 = 7_500_000;
    
    /// Minimum validator stake: 1,000,000 FAT
    pub const MIN_VALIDATOR_STAKE: u128 = 1_000_000 * ONE_FAT;
    
    /// Minimum emission floor: 1 million FAT/year
    pub const MINIMUM_ANNUAL_EMISSION: u128 = 1_000_000 * ONE_FAT;
    
    /// Target equilibrium validators
    pub const TARGET_VALIDATORS: u64 = 5_000;
    
    /// Target APY at equilibrium: 5%
    pub const TARGET_APY_PERCENT: f64 = 5.0;
    
    /// Maximum performance multiplier
    pub const MAX_PERFORMANCE_MULTIPLIER: f64 = 2.0;
    
    /// Minimum performance multiplier (poor performers)
    pub const MIN_PERFORMANCE_MULTIPLIER: f64 = 0.3;
}

pub use constants::*;

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_genesis_supply() {
        assert_eq!(GENESIS_SUPPLY, 10_000_000_000 * ONE_FAT);
    }
    
    #[test]
    fn test_emission_era1() {
        assert_eq!(ANNUAL_EMISSION_ERA1, 500_000_000 * ONE_FAT);
    }
}
