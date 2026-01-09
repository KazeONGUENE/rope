//! # Staking System
//!
//! Validator staking requirements and management.
//!
//! ## Stake Requirements
//!
//! | Validator Type | Minimum Stake | Lock Period | Unbond Time |
//! |----------------|---------------|-------------|-------------|
//! | Standard | 1,000,000 FAT | 3 months | 14 days |
//! | Professional | 5,000,000 FAT | 6 months | 14 days |
//! | Enterprise | 25,000,000 FAT | 12 months | 21 days |
//! | Foundation | 100,000,000 FAT | 24 months | 30 days |

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::constants::*;

/// Validator tier based on stake amount
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ValidatorTier {
    /// Standard validator: 1M FAT minimum
    Standard,
    /// Professional validator: 5M FAT minimum
    Professional,
    /// Enterprise validator: 25M FAT minimum
    Enterprise,
    /// Foundation validator: 100M FAT minimum
    Foundation,
}

impl ValidatorTier {
    /// Get minimum stake for tier
    pub fn minimum_stake(&self) -> u128 {
        match self {
            Self::Standard => 1_000_000 * ONE_FAT,
            Self::Professional => 5_000_000 * ONE_FAT,
            Self::Enterprise => 25_000_000 * ONE_FAT,
            Self::Foundation => 100_000_000 * ONE_FAT,
        }
    }
    
    /// Get minimum lock period in seconds
    pub fn minimum_lock_period(&self) -> u64 {
        match self {
            Self::Standard => 90 * 24 * 3600,      // 3 months
            Self::Professional => 180 * 24 * 3600, // 6 months
            Self::Enterprise => 365 * 24 * 3600,   // 12 months
            Self::Foundation => 730 * 24 * 3600,   // 24 months
        }
    }
    
    /// Get unbonding period in seconds
    pub fn unbonding_period(&self) -> u64 {
        match self {
            Self::Standard => 14 * 24 * 3600,  // 14 days
            Self::Professional => 14 * 24 * 3600, // 14 days
            Self::Enterprise => 21 * 24 * 3600,   // 21 days
            Self::Foundation => 30 * 24 * 3600,   // 30 days
        }
    }
    
    /// Get reward multiplier for tier
    pub fn reward_multiplier(&self) -> f64 {
        match self {
            Self::Standard => 1.0,
            Self::Professional => 1.1,
            Self::Enterprise => 1.2,
            Self::Foundation => 1.3,
        }
    }
    
    /// Get tier from stake amount
    pub fn from_stake(stake: u128) -> Self {
        if stake >= 100_000_000 * ONE_FAT {
            Self::Foundation
        } else if stake >= 25_000_000 * ONE_FAT {
            Self::Enterprise
        } else if stake >= 5_000_000 * ONE_FAT {
            Self::Professional
        } else {
            Self::Standard
        }
    }
    
    /// Get tier name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Professional => "Professional",
            Self::Enterprise => "Enterprise",
            Self::Foundation => "Foundation",
        }
    }
}

/// Stake requirements for different participant types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StakeRequirements {
    /// Minimum stake to become validator
    pub validator_minimum: u128,
    /// Minimum stake to become databox node
    pub databox_minimum: u128,
    /// Minimum stake to create federation
    pub federation_minimum: u128,
    /// Minimum stake to create community
    pub community_minimum: u128,
    /// Maximum stake per validator (for decentralization)
    pub validator_maximum: u128,
}

impl Default for StakeRequirements {
    fn default() -> Self {
        Self {
            validator_minimum: 1_000_000 * ONE_FAT,
            databox_minimum: 100_000 * ONE_FAT,
            federation_minimum: 10_000_000 * ONE_FAT,
            community_minimum: 1_000_000 * ONE_FAT,
            validator_maximum: 1_000_000_000 * ONE_FAT, // 1% of genesis
        }
    }
}

/// Validator stake information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorStake {
    /// Validator ID
    pub validator_id: [u8; 32],
    
    /// Owner address
    pub owner: [u8; 32],
    
    /// Total staked amount
    pub staked_amount: u128,
    
    /// Locked amount (not yet unlockable)
    pub locked_amount: u128,
    
    /// Unbonding amount
    pub unbonding_amount: u128,
    
    /// Unbonding completion time
    pub unbonding_time: Option<i64>,
    
    /// Stake timestamp
    pub stake_time: i64,
    
    /// Lock end timestamp
    pub lock_end_time: i64,
    
    /// Validator tier
    pub tier: ValidatorTier,
    
    /// Total rewards claimed
    pub total_rewards_claimed: u128,
    
    /// Pending rewards (not yet claimed)
    pub pending_rewards: u128,
    
    /// Is validator active
    pub is_active: bool,
    
    /// Slash count
    pub slash_count: u64,
}

impl ValidatorStake {
    /// Create new validator stake
    pub fn new(validator_id: [u8; 32], owner: [u8; 32], amount: u128, timestamp: i64) -> Self {
        let tier = ValidatorTier::from_stake(amount);
        let lock_duration = tier.minimum_lock_period();
        
        Self {
            validator_id,
            owner,
            staked_amount: amount,
            locked_amount: amount,
            unbonding_amount: 0,
            unbonding_time: None,
            stake_time: timestamp,
            lock_end_time: timestamp + lock_duration as i64,
            tier,
            total_rewards_claimed: 0,
            pending_rewards: 0,
            is_active: true,
            slash_count: 0,
        }
    }
    
    /// Check if stake meets minimum requirement
    pub fn meets_minimum(&self) -> bool {
        self.staked_amount >= MIN_VALIDATOR_STAKE
    }
    
    /// Check if stake is unlocked
    pub fn is_unlocked(&self, timestamp: i64) -> bool {
        timestamp >= self.lock_end_time
    }
    
    /// Get effective stake (excluding unbonding)
    pub fn effective_stake(&self) -> u128 {
        self.staked_amount.saturating_sub(self.unbonding_amount)
    }
    
    /// Add more stake
    pub fn add_stake(&mut self, amount: u128, timestamp: i64) {
        self.staked_amount += amount;
        self.locked_amount += amount;
        
        // Update tier
        let new_tier = ValidatorTier::from_stake(self.staked_amount);
        if new_tier != self.tier {
            self.tier = new_tier;
            // Extend lock period for upgrade
            let lock_duration = new_tier.minimum_lock_period();
            let new_lock_end = timestamp + lock_duration as i64;
            self.lock_end_time = self.lock_end_time.max(new_lock_end);
        }
    }
    
    /// Begin unbonding process
    pub fn begin_unbonding(&mut self, amount: u128, timestamp: i64) -> Result<(), StakeError> {
        if !self.is_unlocked(timestamp) {
            return Err(StakeError::StakeLocked);
        }
        
        let available = self.staked_amount - self.unbonding_amount;
        if amount > available {
            return Err(StakeError::InsufficientBalance);
        }
        
        // Check minimum stake maintained
        let remaining = available - amount;
        if remaining > 0 && remaining < MIN_VALIDATOR_STAKE {
            return Err(StakeError::BelowMinimum);
        }
        
        self.unbonding_amount += amount;
        self.unbonding_time = Some(timestamp + self.tier.unbonding_period() as i64);
        
        if remaining < MIN_VALIDATOR_STAKE {
            self.is_active = false;
        }
        
        Ok(())
    }
    
    /// Complete unbonding (withdraw)
    pub fn complete_unbonding(&mut self, timestamp: i64) -> Result<u128, StakeError> {
        let unbond_time = self.unbonding_time.ok_or(StakeError::NotUnbonding)?;
        
        if timestamp < unbond_time {
            return Err(StakeError::UnbondingInProgress);
        }
        
        let amount = self.unbonding_amount;
        self.staked_amount -= amount;
        self.locked_amount = self.locked_amount.saturating_sub(amount);
        self.unbonding_amount = 0;
        self.unbonding_time = None;
        
        // Update tier
        self.tier = ValidatorTier::from_stake(self.staked_amount);
        
        Ok(amount)
    }
    
    /// Add pending rewards
    pub fn add_rewards(&mut self, amount: u128) {
        self.pending_rewards += amount;
    }
    
    /// Claim pending rewards
    pub fn claim_rewards(&mut self) -> u128 {
        let amount = self.pending_rewards;
        self.pending_rewards = 0;
        self.total_rewards_claimed += amount;
        amount
    }
    
    /// Apply slash penalty
    pub fn apply_slash(&mut self, penalty: u128) {
        let slash_amount = penalty.min(self.staked_amount);
        self.staked_amount -= slash_amount;
        self.locked_amount = self.locked_amount.saturating_sub(slash_amount);
        self.slash_count += 1;
        
        // Update tier and status
        self.tier = ValidatorTier::from_stake(self.staked_amount);
        if self.staked_amount < MIN_VALIDATOR_STAKE {
            self.is_active = false;
        }
    }
    
    /// Calculate APY for this stake
    pub fn calculate_apy(&self, total_staked: u128, annual_emission: u128) -> f64 {
        if self.staked_amount == 0 || total_staked == 0 {
            return 0.0;
        }
        
        // Validator pool is 75% of emission
        let validator_pool = annual_emission * 75 / 100;
        
        // Proportional share
        let share = validator_pool as f64 * (self.staked_amount as f64 / total_staked as f64);
        
        // Apply tier multiplier
        let adjusted_share = share * self.tier.reward_multiplier();
        
        // APY
        (adjusted_share / self.staked_amount as f64) * 100.0
    }
}

/// Stake manager
#[derive(Default)]
pub struct StakeManager {
    /// All validator stakes
    validators: HashMap<[u8; 32], ValidatorStake>,
    
    /// Total staked
    pub total_staked: u128,
    
    /// Requirements
    pub requirements: StakeRequirements,
}

impl StakeManager {
    /// Create new stake manager
    pub fn new() -> Self {
        Self::default()
    }
    
    /// Register new validator stake
    pub fn register_validator(
        &mut self,
        validator_id: [u8; 32],
        owner: [u8; 32],
        amount: u128,
        timestamp: i64,
    ) -> Result<ValidatorStake, StakeError> {
        if amount < self.requirements.validator_minimum {
            return Err(StakeError::BelowMinimum);
        }
        
        if self.validators.contains_key(&validator_id) {
            return Err(StakeError::AlreadyRegistered);
        }
        
        let stake = ValidatorStake::new(validator_id, owner, amount, timestamp);
        self.validators.insert(validator_id, stake.clone());
        self.total_staked += amount;
        
        Ok(stake)
    }
    
    /// Get validator stake
    pub fn get_validator(&self, validator_id: &[u8; 32]) -> Option<&ValidatorStake> {
        self.validators.get(validator_id)
    }
    
    /// Get mutable validator stake
    pub fn get_validator_mut(&mut self, validator_id: &[u8; 32]) -> Option<&mut ValidatorStake> {
        self.validators.get_mut(validator_id)
    }
    
    /// Get all active validators
    pub fn active_validators(&self) -> Vec<&ValidatorStake> {
        self.validators.values().filter(|v| v.is_active).collect()
    }
    
    /// Get validator count by tier
    pub fn count_by_tier(&self) -> HashMap<ValidatorTier, u64> {
        let mut counts = HashMap::new();
        for v in self.validators.values() {
            *counts.entry(v.tier).or_insert(0) += 1;
        }
        counts
    }
    
    /// Calculate network APY
    pub fn network_apy(&self, annual_emission: u128) -> f64 {
        if self.total_staked == 0 {
            return 0.0;
        }
        
        let validator_pool = annual_emission * 75 / 100;
        (validator_pool as f64 / self.total_staked as f64) * 100.0
    }
}

/// Stake errors
#[derive(Clone, Debug, thiserror::Error)]
pub enum StakeError {
    #[error("Stake amount below minimum")]
    BelowMinimum,
    
    #[error("Insufficient balance")]
    InsufficientBalance,
    
    #[error("Stake is locked")]
    StakeLocked,
    
    #[error("Not currently unbonding")]
    NotUnbonding,
    
    #[error("Unbonding in progress")]
    UnbondingInProgress,
    
    #[error("Validator already registered")]
    AlreadyRegistered,
    
    #[error("Validator not found")]
    NotFound,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_validator_tiers() {
        assert_eq!(ValidatorTier::from_stake(500_000 * ONE_FAT), ValidatorTier::Standard);
        assert_eq!(ValidatorTier::from_stake(1_000_000 * ONE_FAT), ValidatorTier::Standard);
        assert_eq!(ValidatorTier::from_stake(5_000_000 * ONE_FAT), ValidatorTier::Professional);
        assert_eq!(ValidatorTier::from_stake(25_000_000 * ONE_FAT), ValidatorTier::Enterprise);
        assert_eq!(ValidatorTier::from_stake(100_000_000 * ONE_FAT), ValidatorTier::Foundation);
    }
    
    #[test]
    fn test_validator_stake() {
        let validator_id = [1u8; 32];
        let owner = [2u8; 32];
        let amount = 1_000_000 * ONE_FAT;
        
        let stake = ValidatorStake::new(validator_id, owner, amount, 0);
        
        assert!(stake.meets_minimum());
        assert!(!stake.is_unlocked(0));
        assert!(stake.is_active);
        assert_eq!(stake.tier, ValidatorTier::Standard);
    }
    
    #[test]
    fn test_stake_manager() {
        let mut manager = StakeManager::new();
        
        let result = manager.register_validator(
            [1u8; 32],
            [2u8; 32],
            1_000_000 * ONE_FAT,
            0,
        );
        
        assert!(result.is_ok());
        assert_eq!(manager.total_staked, 1_000_000 * ONE_FAT);
        assert_eq!(manager.active_validators().len(), 1);
    }
    
    #[test]
    fn test_below_minimum_rejected() {
        let mut manager = StakeManager::new();
        
        let result = manager.register_validator(
            [1u8; 32],
            [2u8; 32],
            500_000 * ONE_FAT, // Below minimum
            0,
        );
        
        assert!(matches!(result, Err(StakeError::BelowMinimum)));
    }
}
