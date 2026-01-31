//! # Reward Calculation
//!
//! Calculates rewards for validators and node operators based on performance.
//!
//! ## Reward Types
//!
//! 1. **Anchor Proposer Reward** (30%): Validator who creates anchor
//! 2. **Testimony Rewards** (45%): Validators providing testimonies
//! 3. **Node Operator Rewards** (20%): Storage and bandwidth providers
//! 4. **Federation/Community Pool** (5%): Activity-based distribution

use crate::constants::*;
use crate::emission::{AnchorReward, EmissionSchedule};
use crate::performance::{PerformanceMetrics, PerformanceMultiplier, PerformanceScore};
use crate::staking::ValidatorStake;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Validator reward for an epoch
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidatorReward {
    /// Validator ID
    pub validator_id: [u8; 32],

    /// Epoch number
    pub epoch: u64,

    /// Proposer rewards earned
    pub proposer_rewards: u128,

    /// Testimony rewards earned
    pub testimony_rewards: u128,

    /// Total rewards before multiplier
    pub base_total: u128,

    /// Performance multiplier applied
    pub performance_multiplier: f64,

    /// Final reward after multiplier
    pub final_reward: u128,

    /// Timestamp
    pub timestamp: i64,
}

/// Node operator reward for an epoch
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeReward {
    /// Node ID
    pub node_id: [u8; 32],

    /// Epoch number
    pub epoch: u64,

    /// Storage rewards (based on TB stored)
    pub storage_rewards: u128,

    /// Bandwidth rewards (based on GB served)
    pub bandwidth_rewards: u128,

    /// Regeneration participation rewards
    pub regeneration_rewards: u128,

    /// Total rewards
    pub total_reward: u128,

    /// Timestamp
    pub timestamp: i64,
}

/// Epoch reward distribution summary
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EpochRewardSummary {
    /// Epoch number
    pub epoch: u64,

    /// Total rewards distributed
    pub total_distributed: u128,

    /// Total proposer rewards
    pub total_proposer_rewards: u128,

    /// Total testimony rewards
    pub total_testimony_rewards: u128,

    /// Total node operator rewards
    pub total_node_rewards: u128,

    /// Total federation/community rewards
    pub total_federation_rewards: u128,

    /// Number of validators rewarded
    pub validators_rewarded: u64,

    /// Number of nodes rewarded
    pub nodes_rewarded: u64,

    /// Average validator reward
    pub avg_validator_reward: u128,

    /// Timestamp
    pub timestamp: i64,
}

/// Reward calculator
pub struct RewardCalculator {
    /// Emission schedule
    emission: EmissionSchedule,

    /// Validator stakes
    validator_stakes: HashMap<[u8; 32], ValidatorStake>,

    /// Performance scores
    performance_scores: HashMap<[u8; 32], PerformanceScore>,

    /// Current epoch
    current_epoch: u64,
}

impl RewardCalculator {
    /// Create new reward calculator
    pub fn new(emission: EmissionSchedule) -> Self {
        Self {
            emission,
            validator_stakes: HashMap::new(),
            performance_scores: HashMap::new(),
            current_epoch: 0,
        }
    }

    /// Register a validator
    pub fn register_validator(&mut self, validator_id: [u8; 32], stake: ValidatorStake) {
        self.validator_stakes.insert(validator_id, stake);
    }

    /// Update performance score for a validator/node
    pub fn update_performance(&mut self, node_id: [u8; 32], score: PerformanceScore) {
        self.performance_scores.insert(node_id, score);
    }

    /// Calculate reward for anchor proposer
    pub fn calculate_proposer_reward(&self, validator_id: [u8; 32], timestamp: i64) -> u128 {
        let anchor_dist = self.emission.get_anchor_reward_distribution(timestamp);
        let base_reward = anchor_dist.proposer_share;

        // Apply performance multiplier
        let multiplier = self.get_performance_multiplier(&validator_id);

        (base_reward as f64 * multiplier) as u128
    }

    /// Calculate testimony reward share for a validator
    pub fn calculate_testimony_reward(
        &self,
        validator_id: [u8; 32],
        testimonies_in_anchor: u64,
        total_testimonies_in_anchor: u64,
        timestamp: i64,
    ) -> u128 {
        if total_testimonies_in_anchor == 0 {
            return 0;
        }

        let anchor_dist = self.emission.get_anchor_reward_distribution(timestamp);
        let pool = anchor_dist.testimony_pool;

        // Proportional share based on testimony count
        let share = pool * testimonies_in_anchor as u128 / total_testimonies_in_anchor as u128;

        // Apply performance multiplier
        let multiplier = self.get_performance_multiplier(&validator_id);

        (share as f64 * multiplier) as u128
    }

    /// Calculate node operator reward
    pub fn calculate_node_reward(
        &self,
        node_id: [u8; 32],
        metrics: &PerformanceMetrics,
        timestamp: i64,
    ) -> NodeReward {
        let anchor_dist = self.emission.get_anchor_reward_distribution(timestamp);
        let pool = anchor_dist.node_operator_pool;

        // Calculate individual reward components
        // Storage: based on TB stored
        let storage_weight = (metrics.storage_tb as f64).sqrt();

        // Bandwidth: based on Gbps provided
        let bandwidth_weight = (metrics.bandwidth_gbps as f64).sqrt();

        // Regeneration: based on participation
        let regen_weight = (metrics.strings_stored as f64).sqrt() * 0.1;

        let total_weight = storage_weight + bandwidth_weight + regen_weight;

        // Simple proportional distribution (in real system, would be based on all nodes)
        let storage_rewards = if total_weight > 0.0 {
            (pool as f64 * storage_weight / total_weight / 100.0) as u128
        } else {
            0
        };

        let bandwidth_rewards = if total_weight > 0.0 {
            (pool as f64 * bandwidth_weight / total_weight / 100.0) as u128
        } else {
            0
        };

        let regeneration_rewards = if total_weight > 0.0 {
            (pool as f64 * regen_weight / total_weight / 100.0) as u128
        } else {
            0
        };

        NodeReward {
            node_id,
            epoch: self.current_epoch,
            storage_rewards,
            bandwidth_rewards,
            regeneration_rewards,
            total_reward: storage_rewards + bandwidth_rewards + regeneration_rewards,
            timestamp,
        }
    }

    /// Get performance multiplier for a node
    fn get_performance_multiplier(&self, node_id: &[u8; 32]) -> f64 {
        self.performance_scores
            .get(node_id)
            .map(|s| s.multiplier())
            .unwrap_or(1.0)
    }

    /// Calculate validator APY based on current network state
    pub fn calculate_validator_apy(&self, stake: u128, total_staked: u128, timestamp: i64) -> f64 {
        let annual_emission = self.emission.current_annual_emission(timestamp);

        // Validator pool is proposer (30%) + testimony (45%) = 75% of emission
        let validator_pool = annual_emission * 75 / 100;

        // Proportional share based on stake
        let share = if total_staked > 0 {
            validator_pool as f64 * (stake as f64 / total_staked as f64)
        } else {
            0.0
        };

        // APY = annual reward / stake
        if stake > 0 {
            (share / stake as f64) * 100.0
        } else {
            0.0
        }
    }

    /// Estimate daily reward for a validator
    pub fn estimate_daily_reward(
        &self,
        validator_id: [u8; 32],
        total_validators: u64,
        timestamp: i64,
    ) -> u128 {
        let annual = self.emission.current_annual_emission(timestamp);
        let daily = annual / 365;

        // Validator pool is 75% (proposer + testimony)
        let validator_pool = daily * 75 / 100;

        // Base share (equal distribution)
        let base_share = if total_validators > 0 {
            validator_pool / total_validators as u128
        } else {
            0
        };

        // Apply performance multiplier
        let multiplier = self.get_performance_multiplier(&validator_id);

        (base_share as f64 * multiplier) as u128
    }

    /// Calculate epoch rewards for all participants
    pub fn calculate_epoch_rewards(
        &mut self,
        anchors_in_epoch: u64,
        validators: &[(([u8; 32], u64, u64))], // (id, anchors_proposed, testimonies)
        timestamp: i64,
    ) -> EpochRewardSummary {
        let anchor_dist = self.emission.get_anchor_reward_distribution(timestamp);
        let total_anchor_reward = anchor_dist.total * anchors_in_epoch as u128;

        let total_proposer_rewards = anchor_dist.proposer_share * anchors_in_epoch as u128;
        let total_testimony_rewards = anchor_dist.testimony_pool * anchors_in_epoch as u128;
        let total_node_rewards = anchor_dist.node_operator_pool * anchors_in_epoch as u128;
        let total_federation_rewards = anchor_dist.federation_pool * anchors_in_epoch as u128;

        let validators_rewarded = validators.len() as u64;
        let avg_validator_reward = if validators_rewarded > 0 {
            (total_proposer_rewards + total_testimony_rewards) / validators_rewarded as u128
        } else {
            0
        };

        self.current_epoch += 1;

        EpochRewardSummary {
            epoch: self.current_epoch - 1,
            total_distributed: total_anchor_reward,
            total_proposer_rewards,
            total_testimony_rewards,
            total_node_rewards,
            total_federation_rewards,
            validators_rewarded,
            nodes_rewarded: 0, // Would be calculated separately
            avg_validator_reward,
            timestamp,
        }
    }
}

/// Reward distribution for an anchor block
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnchorRewardDistribution {
    /// Anchor ID
    pub anchor_id: [u8; 32],

    /// Proposer validator ID
    pub proposer_id: [u8; 32],

    /// Proposer reward
    pub proposer_reward: u128,

    /// Testimony validators and their rewards
    pub testimony_rewards: Vec<([u8; 32], u128)>,

    /// Node operator rewards
    pub node_rewards: Vec<([u8; 32], u128)>,

    /// Federation/community pool allocation
    pub federation_allocation: u128,

    /// Total distributed
    pub total_distributed: u128,

    /// Timestamp
    pub timestamp: i64,
}

impl AnchorRewardDistribution {
    /// Create distribution for an anchor
    pub fn new(
        anchor_id: [u8; 32],
        proposer_id: [u8; 32],
        anchor_reward: &AnchorReward,
        testimony_validators: &[([u8; 32], u64)], // (validator_id, testimony_count)
        performance_scores: &HashMap<[u8; 32], PerformanceScore>,
        timestamp: i64,
    ) -> Self {
        // Calculate proposer reward with performance multiplier
        let proposer_multiplier = performance_scores
            .get(&proposer_id)
            .map(|s| s.multiplier())
            .unwrap_or(1.0);
        let proposer_reward = (anchor_reward.proposer_share as f64 * proposer_multiplier) as u128;

        // Calculate testimony rewards
        let total_testimonies: u64 = testimony_validators.iter().map(|(_, c)| c).sum();
        let testimony_rewards: Vec<([u8; 32], u128)> = testimony_validators
            .iter()
            .map(|(id, count)| {
                let share = if total_testimonies > 0 {
                    anchor_reward.testimony_pool * *count as u128 / total_testimonies as u128
                } else {
                    0
                };
                let multiplier = performance_scores
                    .get(id)
                    .map(|s| s.multiplier())
                    .unwrap_or(1.0);
                (*id, (share as f64 * multiplier) as u128)
            })
            .collect();

        let total_distributed = proposer_reward
            + testimony_rewards.iter().map(|(_, r)| r).sum::<u128>()
            + anchor_reward.federation_pool;

        Self {
            anchor_id,
            proposer_id,
            proposer_reward,
            testimony_rewards,
            node_rewards: Vec::new(), // Would be calculated separately
            federation_allocation: anchor_reward.federation_pool,
            total_distributed,
            timestamp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reward_calculator() {
        let emission = EmissionSchedule::new(0);
        let calculator = RewardCalculator::new(emission);

        // At genesis, anchor reward should be ~66.6 FAT
        let anchor_dist = calculator.emission.get_anchor_reward_distribution(0);
        let expected_total = 500_000_000 * ONE_FAT / 7_500_000;

        assert_eq!(anchor_dist.total, expected_total);
    }

    #[test]
    fn test_proposer_reward() {
        let emission = EmissionSchedule::new(0);
        let calculator = RewardCalculator::new(emission);

        let reward = calculator.calculate_proposer_reward([0u8; 32], 0);

        // Proposer gets 30% of anchor reward (allow for small rounding)
        let anchor_dist = calculator.emission.get_anchor_reward_distribution(0);
        let diff = if reward > anchor_dist.proposer_share {
            reward - anchor_dist.proposer_share
        } else {
            anchor_dist.proposer_share - reward
        };
        assert!(diff <= 1, "Reward diff too large: {}", diff);
    }

    #[test]
    fn test_validator_apy() {
        let emission = EmissionSchedule::new(0);
        let calculator = RewardCalculator::new(emission);

        // If 100 validators each stake 1M FAT = 100M total staked
        let stake = 1_000_000 * ONE_FAT;
        let total_staked = 100_000_000 * ONE_FAT;

        let apy = calculator.calculate_validator_apy(stake, total_staked, 0);

        // APY should be high with few validators
        // 75% of 500M = 375M for validators
        // 375M / 100M total stake = 375% APY
        assert!(apy > 100.0);
    }
}
