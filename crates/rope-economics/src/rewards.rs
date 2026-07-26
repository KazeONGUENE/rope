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

use crate::emission::{AnchorReward, EmissionSchedule};
use crate::performance::{PerformanceMetrics, PerformanceScore};
use crate::staking::ValidatorStake;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// M10 (2026-07-25 security audit): apply a floating-point multiplier
/// (e.g. a performance score, typically in
/// `[MIN_PERFORMANCE_MULTIPLIER, MAX_PERFORMANCE_MULTIPLIER]` =
/// `[0.3, 2.0]`) to a large `u128` token amount **without ever routing
/// the large amount itself through `f64`**.
///
/// `f64` has a 52-bit mantissa. Any `u128` amount at or above `2^53`
/// (~9.007e15) silently loses low-order bits the instant it is cast
/// `as f64` — and every anchor-reward-scale amount in this module is
/// well above that threshold once FAT's 18-decimal wei scaling is
/// applied (e.g. ~66.6 FAT at genesis is ~6.66e19 wei). The previous
/// pattern, `(base_reward as f64 * multiplier) as u128`, therefore
/// rounds the reward itself through a lossy float round-trip on every
/// call. IEEE-754 basic arithmetic is deterministic per the standard,
/// but relying on that determinism holding bit-for-bit across every
/// architecture / compiler / optimization-level combination a future
/// validator binary might be built with (e.g. FMA fusion, x87 extended
/// precision on 32-bit targets) is exactly the kind of foot-gun a
/// multi-validator consensus system must not depend on for values that
/// end up minted on-chain.
///
/// The multiplier itself is always small (bounded in practice to
/// `[0.0, 100.0]` by the clamp below), so converting *it* — not the
/// large amount — through `f64` loses nothing meaningful: this function
/// fixed-points the multiplier to parts-per-billion and performs the
/// actual token-scale multiplication in pure `u128`, which is exact and
/// platform-independent.
///
/// `RewardCalculator` is not yet wired into the live consensus/anchor
/// path (see `crates/rope-node` — no call site references it as of
/// this fix), so this change carries zero risk of altering any
/// already-anchored, already-observed reward amount; it hardens the
/// arithmetic before this module's outputs are ever consensus-critical.
pub fn apply_multiplier_ppb(base: u128, multiplier: f64) -> u128 {
    const PPB: u128 = 1_000_000_000;
    // Defensive clamp: NaN/negative/infinite multipliers (e.g. from a
    // future misconfigured `PerformanceScore`) must never be able to
    // mint an unbounded amount or wrap around via a negative-as-unsigned
    // cast. 100.0 is a generous ceiling — no defined multiplier in this
    // crate today exceeds `MAX_PERFORMANCE_MULTIPLIER` (2.0).
    let clamped = if multiplier.is_finite() {
        multiplier.clamp(0.0, 100.0)
    } else {
        0.0
    };
    // `clamped * PPB` is at most 100.0 * 1e9 = 1e11, which fits exactly
    // in an f64's 52-bit mantissa (max exact integer ~9.007e15) — no
    // precision is lost converting the *multiplier*, only the *amount*
    // is protected from ever being cast through f64.
    let multiplier_ppb = (clamped * PPB as f64).round() as u128;
    base.saturating_mul(multiplier_ppb) / PPB
}

/// M10: same rationale as [`apply_multiplier_ppb`], for the common case
/// of applying a proportional weight ratio (`numerator / denominator`,
/// both small `f64` weights such as `sqrt(storage_tb)`) to a large
/// `u128` pool amount. The ratio is computed and clamped to `[0.0, 1.0]`
/// in `f64` (cheap, exact enough for a weighting ratio) before the
/// actual pool-scale multiplication happens in pure `u128` via
/// [`apply_multiplier_ppb`].
pub fn apply_ratio_ppb(base: u128, numerator: f64, denominator: f64) -> u128 {
    if !numerator.is_finite() || !denominator.is_finite() || numerator < 0.0 || denominator <= 0.0
    {
        return 0;
    }
    let ratio = (numerator / denominator).clamp(0.0, 1.0);
    apply_multiplier_ppb(base, ratio)
}

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

        // M10 (2026-07-25 audit): integer fixed-point multiply — see
        // `apply_multiplier_ppb` doc comment for why this replaced a
        // direct `(base_reward as f64 * multiplier) as u128` cast.
        apply_multiplier_ppb(base_reward, multiplier)
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

        // M10: see `apply_multiplier_ppb` doc comment.
        apply_multiplier_ppb(share, multiplier)
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
        let storage_weight = metrics.storage_tb.sqrt();

        // Bandwidth: based on Gbps provided
        let bandwidth_weight = metrics.bandwidth_gbps.sqrt();

        // Regeneration: based on participation
        let regen_weight = (metrics.strings_stored as f64).sqrt() * 0.1;

        let total_weight = storage_weight + bandwidth_weight + regen_weight;

        // Simple proportional distribution (in real system, would be based on all nodes)
        // M10 (2026-07-25 audit): `apply_ratio_ppb` computes the small
        // weight ratio in f64 (safe — these are all small sqrt() values)
        // and then multiplies the large `pool` amount in pure u128,
        // instead of casting `pool` itself through f64. The trailing
        // `/ 100` reproduces the original code's extra 1% scale-down
        // exactly, applied to the now-exact integer result.
        let storage_rewards = apply_ratio_ppb(pool, storage_weight, total_weight) / 100;
        let bandwidth_rewards = apply_ratio_ppb(pool, bandwidth_weight, total_weight) / 100;
        let regeneration_rewards = apply_ratio_ppb(pool, regen_weight, total_weight) / 100;

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

        // M10: see `apply_multiplier_ppb` doc comment.
        apply_multiplier_ppb(base_share, multiplier)
    }

    /// Calculate epoch rewards for all participants
    pub fn calculate_epoch_rewards(
        &mut self,
        anchors_in_epoch: u64,
        validators: &[([u8; 32], u64, u64)], // (id, anchors_proposed, testimonies)
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
        // M10 (2026-07-25 audit): see `apply_multiplier_ppb` doc comment.
        let proposer_reward = apply_multiplier_ppb(anchor_reward.proposer_share, proposer_multiplier);

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
                // M10: see `apply_multiplier_ppb` doc comment.
                (*id, apply_multiplier_ppb(share, multiplier))
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
    use crate::constants::ONE_FAT;

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

    /// M10 (2026-07-25 audit) regression: `apply_multiplier_ppb` must not
    /// lose precision on amounts well above `f64`'s 2^53 exact-integer
    /// ceiling, unlike the `(x as f64 * m) as u128` pattern it replaced.
    #[test]
    fn test_apply_multiplier_ppb_no_precision_loss_on_large_amounts() {
        // 66.6 FAT at 18 decimals ~= 6.66e19 — comfortably above 2^53
        // (~9.007e15), the exact-integer ceiling for f64.
        let base = 500_000_000u128 * ONE_FAT / 7_500_000;
        assert!(base > (1u128 << 53), "test amount must exceed f64's exact-integer range");

        // multiplier = 1.0 must be an exact no-op — the old float path
        // could round this to base ± several thousand wei purely from
        // the u128 -> f64 -> u128 round-trip.
        assert_eq!(apply_multiplier_ppb(base, 1.0), base);

        // multiplier = 0.0 must zero out exactly.
        assert_eq!(apply_multiplier_ppb(base, 0.0), 0);

        // multiplier = 2.0 (MAX_PERFORMANCE_MULTIPLIER) must double exactly.
        assert_eq!(apply_multiplier_ppb(base, 2.0), base * 2);

        // A representative fractional multiplier (1.15) should match the
        // exact rational result to within 1 part-per-billion granularity,
        // not drift by the thousands-of-wei margin a lossy f64 round-trip
        // of `base` itself would introduce.
        let result = apply_multiplier_ppb(base, 1.15);
        let expected = base * 115 / 100;
        let diff = result.abs_diff(expected);
        assert!(
            diff <= base / 1_000_000_000 + 1,
            "fixed-point result {result} drifted too far from exact {expected} (diff {diff})"
        );
    }

    /// M10 regression: defensive clamping on NaN/negative/infinite inputs
    /// — a future misconfigured `PerformanceScore` must never be able to
    /// mint an unbounded amount or wrap around via a negative-as-unsigned
    /// cast through this helper.
    #[test]
    fn test_apply_multiplier_ppb_rejects_degenerate_inputs() {
        let base = 1_000_000u128 * ONE_FAT;
        // Non-finite inputs (NaN, +/-Infinity) fail the `is_finite()`
        // guard entirely and are treated as 0.0, not merely clamped —
        // there is no sane finite ceiling to substitute for "infinity".
        assert_eq!(apply_multiplier_ppb(base, f64::NAN), 0);
        assert_eq!(apply_multiplier_ppb(base, f64::INFINITY), 0);
        assert_eq!(apply_multiplier_ppb(base, f64::NEG_INFINITY), 0);
        // A finite-but-absurd multiplier is clamped to the 100.0 ceiling
        // rather than rejected outright.
        assert_eq!(apply_multiplier_ppb(base, 1_000_000.0), base * 100);
        assert_eq!(apply_multiplier_ppb(base, -5.0), 0);
    }

    /// M10 regression: `apply_ratio_ppb` must reject a zero/negative/NaN
    /// denominator (the `total_weight == 0.0` case in
    /// `calculate_node_reward`) by returning 0, not panicking or dividing
    /// by zero.
    #[test]
    fn test_apply_ratio_ppb_degenerate_denominator() {
        let base = 1_000_000u128 * ONE_FAT;
        assert_eq!(apply_ratio_ppb(base, 1.0, 0.0), 0);
        assert_eq!(apply_ratio_ppb(base, 1.0, f64::NAN), 0);
        assert_eq!(apply_ratio_ppb(base, f64::NAN, 1.0), 0);
        assert_eq!(apply_ratio_ppb(base, 1.0, 1.0), base);
    }
}
