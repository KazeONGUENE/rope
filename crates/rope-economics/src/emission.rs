//! # Emission Schedule
//!
//! Bitcoin-style halving emission schedule for DC FAT.
//!
//! ## Emission Eras
//!
//! ```text
//! Era 1 (2026-2029): 500,000,000 FAT/year  → 66.6 FAT/anchor
//! Era 2 (2030-2033): 250,000,000 FAT/year  → 33.3 FAT/anchor
//! Era 3 (2034-2037): 125,000,000 FAT/year  → 16.65 FAT/anchor
//! Era 4 (2038-2041):  62,500,000 FAT/year  → 8.33 FAT/anchor
//! Era 5 (2042-2045):  31,250,000 FAT/year  → 4.16 FAT/anchor
//! Era 6 (2046-2049):  15,625,000 FAT/year  → 2.08 FAT/anchor
//! ...continues halving until minimum floor...
//! ```
//!
//! ## Total Supply Projection
//!
//! - Genesis (2026): 10B FAT
//! - Year 5 (2030): 12B FAT
//! - Year 10 (2035): 13.5B FAT  
//! - Year 20 (2045): 14.5B FAT
//! - Asymptotic Max: ~18B FAT

use serde::{Deserialize, Serialize};
use crate::constants::*;

/// Emission era information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmissionEra {
    /// Era number (0 = first 4 years)
    pub era: u64,
    /// Start timestamp
    pub start_time: i64,
    /// End timestamp (exclusive)
    pub end_time: i64,
    /// Annual emission in smallest units
    pub annual_emission: u128,
    /// Reward per anchor block
    pub anchor_reward: u128,
}

/// Anchor block reward distribution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnchorReward {
    /// Total reward for this anchor
    pub total: u128,
    /// Anchor proposer share (30%)
    pub proposer_share: u128,
    /// Testimony validators pool (45%)
    pub testimony_pool: u128,
    /// Node operators pool (20%)
    pub node_operator_pool: u128,
    /// Federation/Community pool (5%)
    pub federation_pool: u128,
}

impl AnchorReward {
    /// Create anchor reward from total amount
    pub fn from_total(total: u128) -> Self {
        Self {
            total,
            proposer_share: total * 30 / 100,      // 30%
            testimony_pool: total * 45 / 100,      // 45%
            node_operator_pool: total * 20 / 100,  // 20%
            federation_pool: total * 5 / 100,      // 5%
        }
    }
    
    /// Verify distribution sums to total (accounting for rounding)
    pub fn verify(&self) -> bool {
        let sum = self.proposer_share 
            + self.testimony_pool 
            + self.node_operator_pool 
            + self.federation_pool;
        // Allow for small rounding differences
        sum <= self.total && self.total - sum < 100
    }
}

/// Emission schedule manager (Bitcoin-style halving)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EmissionSchedule {
    /// Genesis timestamp (Unix seconds)
    pub genesis_time: i64,
    /// Initial annual emission (Era 1): 500M FAT
    pub initial_emission: u128,
    /// Halving interval in seconds: 4 years
    pub halving_interval: u64,
    /// Minimum annual emission floor: 1M FAT
    pub minimum_emission: u128,
    /// Total minted so far (excluding genesis)
    pub total_minted: u128,
    /// Current era
    pub current_era: u64,
}

impl Default for EmissionSchedule {
    fn default() -> Self {
        Self::new(chrono::Utc::now().timestamp())
    }
}

impl EmissionSchedule {
    /// Create new emission schedule starting at genesis time
    pub fn new(genesis_time: i64) -> Self {
        Self {
            genesis_time,
            initial_emission: ANNUAL_EMISSION_ERA1,
            halving_interval: HALVING_INTERVAL_SECS,
            minimum_emission: MINIMUM_ANNUAL_EMISSION,
            total_minted: 0,
            current_era: 0,
        }
    }
    
    /// Create emission schedule for mainnet launch
    pub fn mainnet() -> Self {
        // January 1, 2026 00:00:00 UTC
        let genesis = chrono::NaiveDate::from_ymd_opt(2026, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp();
        Self::new(genesis)
    }
    
    /// Get current era based on timestamp
    pub fn get_era(&self, timestamp: i64) -> u64 {
        if timestamp < self.genesis_time {
            return 0;
        }
        let elapsed = (timestamp - self.genesis_time) as u64;
        elapsed / self.halving_interval
    }
    
    /// Get era information
    pub fn get_era_info(&self, era: u64) -> EmissionEra {
        let start_time = self.genesis_time + (era * self.halving_interval) as i64;
        let end_time = start_time + self.halving_interval as i64;
        let annual_emission = self.annual_emission_for_era(era);
        let anchor_reward = self.anchor_reward_for_era(era);
        
        EmissionEra {
            era,
            start_time,
            end_time,
            annual_emission,
            anchor_reward,
        }
    }
    
    /// Get annual emission for a specific era (with halving)
    pub fn annual_emission_for_era(&self, era: u64) -> u128 {
        // Halving: divide by 2^era
        let emission = self.initial_emission >> era;
        emission.max(self.minimum_emission)
    }
    
    /// Get current annual emission
    pub fn current_annual_emission(&self, timestamp: i64) -> u128 {
        let era = self.get_era(timestamp);
        self.annual_emission_for_era(era)
    }
    
    /// Get reward per anchor block for a specific era
    pub fn anchor_reward_for_era(&self, era: u64) -> u128 {
        let annual = self.annual_emission_for_era(era);
        annual / ANCHORS_PER_YEAR as u128
    }
    
    /// Get current anchor reward
    pub fn current_anchor_reward(&self, timestamp: i64) -> u128 {
        let era = self.get_era(timestamp);
        self.anchor_reward_for_era(era)
    }
    
    /// Get full anchor reward distribution for current era
    pub fn get_anchor_reward_distribution(&self, timestamp: i64) -> AnchorReward {
        let total = self.current_anchor_reward(timestamp);
        AnchorReward::from_total(total)
    }
    
    /// Calculate total supply at a given timestamp
    pub fn total_supply_at(&self, timestamp: i64) -> u128 {
        if timestamp < self.genesis_time {
            return GENESIS_SUPPLY;
        }
        
        let mut total = GENESIS_SUPPLY;
        let mut current_time = self.genesis_time;
        
        loop {
            let era = self.get_era(current_time);
            let era_end = self.genesis_time + ((era + 1) * self.halving_interval) as i64;
            let annual_emission = self.annual_emission_for_era(era);
            
            if timestamp >= era_end {
                // Full era emission
                total += annual_emission * 4; // 4 years per era
                current_time = era_end;
            } else {
                // Partial era
                let seconds_in_era = (timestamp - current_time) as u128;
                let seconds_per_year = 365 * 24 * 3600u128;
                let partial_emission = annual_emission * seconds_in_era / seconds_per_year;
                total += partial_emission;
                break;
            }
            
            // Safety: prevent infinite loop
            if era > 50 {
                break;
            }
        }
        
        total
    }
    
    /// Project total supply for major milestones
    pub fn supply_projections(&self) -> Vec<(i64, u128, &'static str)> {
        let genesis = self.genesis_time;
        let year = 365 * 24 * 3600i64;
        
        vec![
            (genesis, GENESIS_SUPPLY, "Genesis (2026)"),
            (genesis + year, self.total_supply_at(genesis + year), "Year 1 (2027)"),
            (genesis + 4 * year, self.total_supply_at(genesis + 4 * year), "Year 4 (2030)"),
            (genesis + 5 * year, self.total_supply_at(genesis + 5 * year), "Year 5 (2031)"),
            (genesis + 10 * year, self.total_supply_at(genesis + 10 * year), "Year 10 (2036)"),
            (genesis + 20 * year, self.total_supply_at(genesis + 20 * year), "Year 20 (2046)"),
            (genesis + 50 * year, self.total_supply_at(genesis + 50 * year), "Year 50 (2076)"),
        ]
    }
    
    /// Calculate asymptotic maximum supply
    /// Sum of geometric series: Genesis + Sum(500M * 4 / 2^n) for n=0 to infinity
    /// = 10B + 2B * Sum(1/2^n) = 10B + 2B * 2 = 10B + 4B = 14B (theoretical)
    /// Plus minimum floor contributions ~= 18B practical max
    pub fn asymptotic_max_supply(&self) -> u128 {
        // Genesis + 4 years of each era until minimum floor
        let mut total = GENESIS_SUPPLY;
        let mut era = 0u64;
        
        loop {
            let annual = self.annual_emission_for_era(era);
            if annual <= self.minimum_emission {
                // At minimum floor, estimate 100 more years
                total += self.minimum_emission * 100;
                break;
            }
            total += annual * 4; // 4 years per era
            era += 1;
            
            // Safety limit
            if era > 50 {
                break;
            }
        }
        
        total
    }
    
    /// Record minted tokens
    pub fn record_mint(&mut self, amount: u128, timestamp: i64) {
        self.total_minted += amount;
        self.current_era = self.get_era(timestamp);
    }
    
    /// Get emission rate per second for current era
    pub fn emission_rate_per_second(&self, timestamp: i64) -> u128 {
        let annual = self.current_annual_emission(timestamp);
        annual / (365 * 24 * 3600)
    }
    
    /// Get inflation rate for current era
    pub fn inflation_rate(&self, timestamp: i64) -> f64 {
        let annual = self.current_annual_emission(timestamp) as f64;
        let total = self.total_supply_at(timestamp) as f64;
        (annual / total) * 100.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_halving_schedule() {
        let schedule = EmissionSchedule::new(0);
        
        // Era 0: 500M
        assert_eq!(schedule.annual_emission_for_era(0), 500_000_000 * ONE_FAT);
        
        // Era 1: 250M (halved)
        assert_eq!(schedule.annual_emission_for_era(1), 250_000_000 * ONE_FAT);
        
        // Era 2: 125M (halved again)
        assert_eq!(schedule.annual_emission_for_era(2), 125_000_000 * ONE_FAT);
        
        // Era 3: 62.5M
        assert_eq!(schedule.annual_emission_for_era(3), 62_500_000 * ONE_FAT);
    }
    
    #[test]
    fn test_anchor_reward() {
        let schedule = EmissionSchedule::new(0);
        
        // Era 0: 500M / 7.5M anchors ≈ 66.6 FAT
        let reward = schedule.anchor_reward_for_era(0);
        let expected = 500_000_000 * ONE_FAT / 7_500_000;
        assert_eq!(reward, expected);
    }
    
    #[test]
    fn test_anchor_reward_distribution() {
        let total = 100 * ONE_FAT;
        let dist = AnchorReward::from_total(total);
        
        assert_eq!(dist.proposer_share, 30 * ONE_FAT);
        assert_eq!(dist.testimony_pool, 45 * ONE_FAT);
        assert_eq!(dist.node_operator_pool, 20 * ONE_FAT);
        assert_eq!(dist.federation_pool, 5 * ONE_FAT);
        assert!(dist.verify());
    }
    
    #[test]
    fn test_era_calculation() {
        let schedule = EmissionSchedule::new(0);
        
        // Year 0-4: Era 0
        assert_eq!(schedule.get_era(0), 0);
        assert_eq!(schedule.get_era(HALVING_INTERVAL_SECS as i64 - 1), 0);
        
        // Year 4-8: Era 1
        assert_eq!(schedule.get_era(HALVING_INTERVAL_SECS as i64), 1);
        
        // Year 8-12: Era 2
        assert_eq!(schedule.get_era(2 * HALVING_INTERVAL_SECS as i64), 2);
    }
    
    #[test]
    fn test_genesis_supply() {
        let schedule = EmissionSchedule::new(1000);
        
        // Before genesis
        assert_eq!(schedule.total_supply_at(500), GENESIS_SUPPLY);
        
        // At genesis
        assert_eq!(schedule.total_supply_at(1000), GENESIS_SUPPLY);
    }
    
    #[test]
    fn test_inflation_rate() {
        let schedule = EmissionSchedule::new(0);
        
        // Era 0: 500M / 10B = 5%
        let inflation = schedule.inflation_rate(0);
        assert!((inflation - 5.0).abs() < 0.1);
    }
}
