//! # Slashing System
//!
//! Penalty system for malicious or negligent behavior.
//!
//! ## Offense Types & Penalties
//!
//! | Offense | Penalty | Jail Duration | Description |
//! |---------|---------|---------------|-------------|
//! | Double Sign | 5% stake | 30 days | Signing conflicting blocks |
//! | Downtime | 0.1% stake | 7 days | Extended unavailability |
//! | Invalid Testimony | 1% stake | 14 days | False AI testimony |
//! | Data Corruption | 10% stake | 60 days | Corrupting stored data |
//! | Collusion | 20% stake | Permanent | Coordinated attack |

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::constants::ONE_FAT;

/// Slashing offense types
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SlashingOffense {
    /// Signing two different blocks at same height
    DoubleSigning,
    /// Extended downtime (>24 hours)
    Downtime,
    /// Providing invalid AI testimony
    InvalidTestimony,
    /// Corrupting stored data
    DataCorruption,
    /// Coordinated attack with other validators
    Collusion,
    /// Missing too many anchor proposals
    MissedProposals,
    /// Failing to participate in regeneration
    RegenerationFailure,
    /// Broadcasting invalid transactions
    InvalidTransactions,
}

impl SlashingOffense {
    /// Get penalty percentage of stake
    pub fn penalty_percent(&self) -> u8 {
        match self {
            Self::DoubleSigning => 5,
            Self::Downtime => 0, // 0.1% handled separately
            Self::InvalidTestimony => 1,
            Self::DataCorruption => 10,
            Self::Collusion => 20,
            Self::MissedProposals => 0, // 0.05% handled separately
            Self::RegenerationFailure => 1,
            Self::InvalidTransactions => 2,
        }
    }
    
    /// Get penalty amount in FAT (for micro-penalties)
    pub fn penalty_fat(&self) -> u128 {
        match self {
            Self::Downtime => 1000 * ONE_FAT, // 1000 FAT per 24h downtime
            Self::MissedProposals => 100 * ONE_FAT, // 100 FAT per missed proposal
            _ => 0,
        }
    }
    
    /// Get jail duration in seconds
    pub fn jail_duration(&self) -> u64 {
        match self {
            Self::DoubleSigning => 30 * 24 * 3600,      // 30 days
            Self::Downtime => 7 * 24 * 3600,            // 7 days
            Self::InvalidTestimony => 14 * 24 * 3600,   // 14 days
            Self::DataCorruption => 60 * 24 * 3600,     // 60 days
            Self::Collusion => u64::MAX,                 // Permanent
            Self::MissedProposals => 3 * 24 * 3600,     // 3 days
            Self::RegenerationFailure => 7 * 24 * 3600, // 7 days
            Self::InvalidTransactions => 7 * 24 * 3600, // 7 days
        }
    }
    
    /// Is this offense permanently disqualifying?
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Collusion)
    }
    
    /// Get offense name
    pub fn name(&self) -> &'static str {
        match self {
            Self::DoubleSigning => "Double Signing",
            Self::Downtime => "Extended Downtime",
            Self::InvalidTestimony => "Invalid Testimony",
            Self::DataCorruption => "Data Corruption",
            Self::Collusion => "Collusion",
            Self::MissedProposals => "Missed Proposals",
            Self::RegenerationFailure => "Regeneration Failure",
            Self::InvalidTransactions => "Invalid Transactions",
        }
    }
    
    /// Get severity level (1-5)
    pub fn severity(&self) -> u8 {
        match self {
            Self::MissedProposals => 1,
            Self::Downtime => 2,
            Self::RegenerationFailure => 2,
            Self::InvalidTransactions => 2,
            Self::InvalidTestimony => 3,
            Self::DoubleSigning => 4,
            Self::DataCorruption => 4,
            Self::Collusion => 5,
        }
    }
}

/// Slashing penalty details
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlashingPenalty {
    /// Penalty ID
    pub penalty_id: [u8; 32],
    
    /// Validator ID
    pub validator_id: [u8; 32],
    
    /// Offense type
    pub offense: SlashingOffense,
    
    /// Amount slashed
    pub slashed_amount: u128,
    
    /// Jail start time
    pub jail_start: i64,
    
    /// Jail end time
    pub jail_end: i64,
    
    /// Evidence hash
    pub evidence_hash: [u8; 32],
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Is executed
    pub is_executed: bool,
}

impl SlashingPenalty {
    /// Check if validator is still jailed
    pub fn is_jailed(&self, timestamp: i64) -> bool {
        timestamp < self.jail_end
    }
    
    /// Calculate penalty amount from stake
    pub fn calculate_penalty(offense: &SlashingOffense, stake: u128) -> u128 {
        let percent_penalty = offense.penalty_percent() as u128;
        let percent_amount = stake * percent_penalty / 100;
        
        // Add fixed FAT penalty if applicable
        let fixed_penalty = offense.penalty_fat();
        
        // Take the larger of percentage or fixed
        percent_amount.max(fixed_penalty)
    }
}

/// Slashing engine
pub struct SlashingEngine {
    /// Pending penalties
    pending: HashMap<[u8; 32], SlashingPenalty>,
    
    /// Executed penalties
    executed: Vec<SlashingPenalty>,
    
    /// Jailed validators
    jailed: HashMap<[u8; 32], i64>, // validator_id -> jail_end
    
    /// Offense counts per validator
    offense_counts: HashMap<[u8; 32], HashMap<SlashingOffense, u64>>,
    
    /// Permanently banned validators
    banned: Vec<[u8; 32]>,
}

impl Default for SlashingEngine {
    fn default() -> Self {
        Self {
            pending: HashMap::new(),
            executed: Vec::new(),
            jailed: HashMap::new(),
            offense_counts: HashMap::new(),
            banned: Vec::new(),
        }
    }
}

impl SlashingEngine {
    /// Create new slashing engine
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Report an offense
    pub fn report_offense(
        &mut self,
        validator_id: [u8; 32],
        offense: SlashingOffense,
        stake: u128,
        evidence_hash: [u8; 32],
        timestamp: i64,
    ) -> SlashingPenalty {
        let penalty_id = blake3::hash(&[
            &validator_id[..],
            &(timestamp as u64).to_le_bytes(),
            &evidence_hash[..],
        ].concat()).into();
        
        let slashed_amount = SlashingPenalty::calculate_penalty(&offense, stake);
        let jail_duration = offense.jail_duration();
        let jail_end = if offense.is_permanent() {
            i64::MAX
        } else {
            timestamp + jail_duration as i64
        };
        
        let penalty = SlashingPenalty {
            penalty_id,
            validator_id,
            offense,
            slashed_amount,
            jail_start: timestamp,
            jail_end,
            evidence_hash,
            timestamp,
            is_executed: false,
        };
        
        // Track offense count
        *self.offense_counts
            .entry(validator_id)
            .or_default()
            .entry(offense)
            .or_insert(0) += 1;
        
        self.pending.insert(penalty_id, penalty.clone());
        
        penalty
    }
    
    /// Execute a pending penalty
    pub fn execute_penalty(&mut self, penalty_id: &[u8; 32]) -> Option<SlashingPenalty> {
        if let Some(mut penalty) = self.pending.remove(penalty_id) {
            penalty.is_executed = true;
            
            // Jail the validator
            self.jailed.insert(penalty.validator_id, penalty.jail_end);
            
            // Ban if permanent
            if penalty.offense.is_permanent() {
                self.banned.push(penalty.validator_id);
            }
            
            self.executed.push(penalty.clone());
            Some(penalty)
        } else {
            None
        }
    }
    
    /// Check if validator is jailed
    pub fn is_jailed(&self, validator_id: &[u8; 32], timestamp: i64) -> bool {
        self.jailed.get(validator_id)
            .map(|&end| timestamp < end)
            .unwrap_or(false)
    }
    
    /// Check if validator is banned
    pub fn is_banned(&self, validator_id: &[u8; 32]) -> bool {
        self.banned.contains(validator_id)
    }
    
    /// Get jail end time for validator
    pub fn jail_end(&self, validator_id: &[u8; 32]) -> Option<i64> {
        self.jailed.get(validator_id).copied()
    }
    
    /// Release validator from jail (if time served)
    pub fn release_if_served(&mut self, validator_id: &[u8; 32], timestamp: i64) -> bool {
        if self.is_banned(validator_id) {
            return false;
        }
        
        if let Some(&end) = self.jailed.get(validator_id) {
            if timestamp >= end {
                self.jailed.remove(validator_id);
                return true;
            }
        }
        false
    }
    
    /// Get offense count for validator
    pub fn offense_count(&self, validator_id: &[u8; 32], offense: &SlashingOffense) -> u64 {
        self.offense_counts
            .get(validator_id)
            .and_then(|counts| counts.get(offense))
            .copied()
            .unwrap_or(0)
    }
    
    /// Get total slashed amount for validator
    pub fn total_slashed(&self, validator_id: &[u8; 32]) -> u128 {
        self.executed.iter()
            .filter(|p| p.validator_id == *validator_id)
            .map(|p| p.slashed_amount)
            .sum()
    }
    
    /// Get all jailed validators
    pub fn jailed_validators(&self, timestamp: i64) -> Vec<[u8; 32]> {
        self.jailed.iter()
            .filter(|(_, &end)| timestamp < end)
            .map(|(&id, _)| id)
            .collect()
    }
    
    /// Check if validator should be escalated (repeat offender)
    pub fn should_escalate(&self, validator_id: &[u8; 32], offense: &SlashingOffense) -> bool {
        let count = self.offense_count(validator_id, offense);
        match offense.severity() {
            1 => count >= 10, // Low severity: 10 strikes
            2 => count >= 5,  // Medium: 5 strikes
            3 => count >= 3,  // High: 3 strikes
            4 => count >= 2,  // Very high: 2 strikes
            5 => count >= 1,  // Critical: 1 strike
            _ => false,
        }
    }
    
    /// Get escalated penalty (next level)
    pub fn escalate_offense(&self, offense: SlashingOffense) -> SlashingOffense {
        match offense {
            SlashingOffense::MissedProposals => SlashingOffense::Downtime,
            SlashingOffense::Downtime => SlashingOffense::RegenerationFailure,
            SlashingOffense::RegenerationFailure => SlashingOffense::InvalidTransactions,
            SlashingOffense::InvalidTransactions => SlashingOffense::InvalidTestimony,
            SlashingOffense::InvalidTestimony => SlashingOffense::DoubleSigning,
            SlashingOffense::DoubleSigning => SlashingOffense::DataCorruption,
            SlashingOffense::DataCorruption => SlashingOffense::Collusion,
            SlashingOffense::Collusion => SlashingOffense::Collusion, // Already max
        }
    }
}

/// Slashed funds distribution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SlashedFundsDistribution {
    /// Total slashed
    pub total_slashed: u128,
    
    /// Burned (removed from circulation): 50%
    pub burned: u128,
    
    /// Insurance fund: 30%
    pub insurance_fund: u128,
    
    /// Reporting reward: 20%
    pub reporter_reward: u128,
}

impl SlashedFundsDistribution {
    /// Calculate distribution from slashed amount
    pub fn from_slashed(amount: u128) -> Self {
        Self {
            total_slashed: amount,
            burned: amount * 50 / 100,
            insurance_fund: amount * 30 / 100,
            reporter_reward: amount * 20 / 100,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_offense_penalties() {
        assert_eq!(SlashingOffense::DoubleSigning.penalty_percent(), 5);
        assert_eq!(SlashingOffense::DataCorruption.penalty_percent(), 10);
        assert_eq!(SlashingOffense::Collusion.penalty_percent(), 20);
    }
    
    #[test]
    fn test_penalty_calculation() {
        let stake = 1_000_000 * ONE_FAT;
        
        let double_sign = SlashingPenalty::calculate_penalty(&SlashingOffense::DoubleSigning, stake);
        assert_eq!(double_sign, 50_000 * ONE_FAT); // 5%
        
        let collusion = SlashingPenalty::calculate_penalty(&SlashingOffense::Collusion, stake);
        assert_eq!(collusion, 200_000 * ONE_FAT); // 20%
    }
    
    #[test]
    fn test_slashing_engine() {
        let mut engine = SlashingEngine::new();
        
        let validator_id = [1u8; 32];
        let evidence = [2u8; 32];
        let stake = 1_000_000 * ONE_FAT;
        let timestamp = 1000;
        
        let penalty = engine.report_offense(
            validator_id,
            SlashingOffense::DoubleSigning,
            stake,
            evidence,
            timestamp,
        );
        
        assert_eq!(penalty.slashed_amount, 50_000 * ONE_FAT);
        assert!(!engine.is_jailed(&validator_id, timestamp)); // Not executed yet
        
        engine.execute_penalty(&penalty.penalty_id);
        assert!(engine.is_jailed(&validator_id, timestamp + 1));
    }
    
    #[test]
    fn test_slashed_distribution() {
        let slashed = 100_000 * ONE_FAT;
        let dist = SlashedFundsDistribution::from_slashed(slashed);
        
        assert_eq!(dist.burned, 50_000 * ONE_FAT);
        assert_eq!(dist.insurance_fund, 30_000 * ONE_FAT);
        assert_eq!(dist.reporter_reward, 20_000 * ONE_FAT);
    }
}
