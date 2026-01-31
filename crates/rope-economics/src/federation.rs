//! # Federation & Community Rewards
//!
//! Activity-based reward distribution for federations and communities.
//!
//! ## Activity Tiers
//!
//! | Tier | Activity Level | Pool Share | Requirements |
//! |------|----------------|------------|--------------|
//! | Platinum | Very High | 40% | 10M+ tx/month, 1M+ users |
//! | Gold | High | 30% | 1M+ tx/month, 100K+ users |
//! | Silver | Medium | 20% | 100K+ tx/month, 10K+ users |
//! | Bronze | Low | 10% | <100K tx/month |

use crate::emission::AnchorReward;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Activity tier classification
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum ActivityTier {
    /// Platinum: Very high activity
    Platinum,
    /// Gold: High activity  
    Gold,
    /// Silver: Medium activity
    Silver,
    /// Bronze: Low activity
    Bronze,
}

impl ActivityTier {
    /// Get pool share percentage for tier
    pub fn pool_share_percent(&self) -> u8 {
        match self {
            Self::Platinum => 40,
            Self::Gold => 30,
            Self::Silver => 20,
            Self::Bronze => 10,
        }
    }

    /// Get reward multiplier for tier
    pub fn reward_multiplier(&self) -> f64 {
        match self {
            Self::Platinum => 2.0,
            Self::Gold => 1.5,
            Self::Silver => 1.0,
            Self::Bronze => 0.5,
        }
    }

    /// Determine tier from activity metrics
    pub fn from_activity(monthly_transactions: u64, active_users: u64) -> Self {
        if monthly_transactions >= 10_000_000 && active_users >= 1_000_000 {
            Self::Platinum
        } else if monthly_transactions >= 1_000_000 && active_users >= 100_000 {
            Self::Gold
        } else if monthly_transactions >= 100_000 && active_users >= 10_000 {
            Self::Silver
        } else {
            Self::Bronze
        }
    }

    /// Get tier name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Platinum => "Platinum",
            Self::Gold => "Gold",
            Self::Silver => "Silver",
            Self::Bronze => "Bronze",
        }
    }
}

/// Federation/Community activity metrics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ActivityMetrics {
    /// Federation/Community ID
    pub entity_id: [u8; 32],

    /// Number of active data wallets
    pub active_wallets: u64,

    /// Monthly transactions
    pub monthly_transactions: u64,

    /// Monthly strings stored
    pub monthly_strings: u64,

    /// Monthly testimonies validated
    pub monthly_testimonies: u64,

    /// Active users in last 30 days
    pub active_users_30d: u64,

    /// Total storage used (GB)
    pub storage_used_gb: u64,

    /// API calls this month
    pub api_calls: u64,

    /// Measurement period start
    pub period_start: i64,

    /// Measurement period end
    pub period_end: i64,
}

impl ActivityMetrics {
    /// Calculate activity score (0-100)
    pub fn activity_score(&self) -> f64 {
        // Weighted score based on multiple metrics
        let tx_score = (self.monthly_transactions as f64).log10().min(10.0) * 10.0;
        let user_score = (self.active_users_30d as f64).log10().min(8.0) * 10.0;
        let storage_score = (self.storage_used_gb as f64).log10().min(5.0) * 10.0;
        let testimony_score = (self.monthly_testimonies as f64).log10().min(7.0) * 10.0;

        (tx_score * 0.4 + user_score * 0.3 + storage_score * 0.15 + testimony_score * 0.15)
            .min(100.0)
    }

    /// Get tier based on metrics
    pub fn tier(&self) -> ActivityTier {
        ActivityTier::from_activity(self.monthly_transactions, self.active_users_30d)
    }
}

/// Federation reward state
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FederationRewardState {
    /// Federation ID
    pub federation_id: [u8; 32],

    /// Federation name
    pub name: String,

    /// Current activity tier
    pub tier: ActivityTier,

    /// Total rewards earned (lifetime)
    pub total_rewards: u128,

    /// Pending rewards
    pub pending_rewards: u128,

    /// Last reward timestamp
    pub last_reward_time: i64,

    /// Activity metrics
    pub metrics: ActivityMetrics,

    /// Is eligible for rewards
    pub is_eligible: bool,
}

/// Federation rewards calculator
pub struct FederationRewards {
    /// Federation states
    federations: HashMap<[u8; 32], FederationRewardState>,

    /// Minimum activity threshold for rewards
    pub min_transactions_threshold: u64,

    /// Minimum users threshold
    pub min_users_threshold: u64,
}

impl Default for FederationRewards {
    fn default() -> Self {
        Self {
            federations: HashMap::new(),
            min_transactions_threshold: 1_000, // Minimum 1000 tx/month
            min_users_threshold: 100,          // Minimum 100 users
        }
    }
}

impl FederationRewards {
    /// Create new federation rewards manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Register federation for rewards
    pub fn register_federation(&mut self, id: [u8; 32], name: String, metrics: ActivityMetrics) {
        let tier = metrics.tier();
        let is_eligible = metrics.monthly_transactions >= self.min_transactions_threshold
            && metrics.active_users_30d >= self.min_users_threshold;

        self.federations.insert(
            id,
            FederationRewardState {
                federation_id: id,
                name,
                tier,
                total_rewards: 0,
                pending_rewards: 0,
                last_reward_time: 0,
                metrics,
                is_eligible,
            },
        );
    }

    /// Update federation metrics
    pub fn update_metrics(&mut self, id: &[u8; 32], metrics: ActivityMetrics) {
        if let Some(state) = self.federations.get_mut(id) {
            state.tier = metrics.tier();
            state.is_eligible = metrics.monthly_transactions >= self.min_transactions_threshold
                && metrics.active_users_30d >= self.min_users_threshold;
            state.metrics = metrics;
        }
    }

    /// Distribute rewards for an epoch
    pub fn distribute_epoch_rewards(
        &mut self,
        anchor_rewards: &[AnchorReward],
        timestamp: i64,
    ) -> Vec<([u8; 32], u128)> {
        // Total federation pool from all anchors
        let total_pool: u128 = anchor_rewards.iter().map(|r| r.federation_pool).sum();

        if total_pool == 0 {
            return Vec::new();
        }

        // Get eligible federations grouped by tier
        let mut by_tier: HashMap<ActivityTier, Vec<[u8; 32]>> = HashMap::new();
        for (id, state) in &self.federations {
            if state.is_eligible {
                by_tier.entry(state.tier).or_default().push(*id);
            }
        }

        let mut distributions = Vec::new();

        // Distribute pool by tier shares
        for tier in [
            ActivityTier::Platinum,
            ActivityTier::Gold,
            ActivityTier::Silver,
            ActivityTier::Bronze,
        ] {
            let tier_share = total_pool * tier.pool_share_percent() as u128 / 100;

            if let Some(fed_ids) = by_tier.get(&tier) {
                if !fed_ids.is_empty() {
                    // Equal distribution within tier (could be weighted by activity score)
                    let per_federation = tier_share / fed_ids.len() as u128;

                    for id in fed_ids {
                        if let Some(state) = self.federations.get_mut(id) {
                            state.pending_rewards += per_federation;
                            state.last_reward_time = timestamp;
                            distributions.push((*id, per_federation));
                        }
                    }
                }
            }
        }

        distributions
    }

    /// Claim pending rewards
    pub fn claim_rewards(&mut self, id: &[u8; 32]) -> u128 {
        if let Some(state) = self.federations.get_mut(id) {
            let amount = state.pending_rewards;
            state.pending_rewards = 0;
            state.total_rewards += amount;
            amount
        } else {
            0
        }
    }

    /// Get federation state
    pub fn get_federation(&self, id: &[u8; 32]) -> Option<&FederationRewardState> {
        self.federations.get(id)
    }

    /// Get all eligible federations
    pub fn eligible_federations(&self) -> Vec<&FederationRewardState> {
        self.federations
            .values()
            .filter(|f| f.is_eligible)
            .collect()
    }

    /// Count federations by tier
    pub fn count_by_tier(&self) -> HashMap<ActivityTier, u64> {
        let mut counts = HashMap::new();
        for state in self.federations.values() {
            *counts.entry(state.tier).or_insert(0) += 1;
        }
        counts
    }
}

/// Community reward state (similar to federation but for communities)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommunityRewardState {
    /// Community ID
    pub community_id: [u8; 32],

    /// Community name
    pub name: String,

    /// Current activity tier
    pub tier: ActivityTier,

    /// Total rewards earned (lifetime)
    pub total_rewards: u128,

    /// Pending rewards
    pub pending_rewards: u128,

    /// Industry sector
    pub industry: String,

    /// Activity metrics
    pub metrics: ActivityMetrics,
}

/// Community rewards manager
pub struct CommunityRewards {
    /// Community states
    communities: HashMap<[u8; 32], CommunityRewardState>,

    /// Minimum activity threshold
    pub min_transactions_threshold: u64,
}

impl Default for CommunityRewards {
    fn default() -> Self {
        Self {
            communities: HashMap::new(),
            min_transactions_threshold: 500, // Lower threshold for communities
        }
    }
}

impl CommunityRewards {
    /// Create new community rewards manager
    pub fn new() -> Self {
        Self::default()
    }

    /// Register community
    pub fn register_community(
        &mut self,
        id: [u8; 32],
        name: String,
        industry: String,
        metrics: ActivityMetrics,
    ) {
        let tier = metrics.tier();

        self.communities.insert(
            id,
            CommunityRewardState {
                community_id: id,
                name,
                tier,
                total_rewards: 0,
                pending_rewards: 0,
                industry,
                metrics,
            },
        );
    }

    /// Get community
    pub fn get_community(&self, id: &[u8; 32]) -> Option<&CommunityRewardState> {
        self.communities.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_activity_tiers() {
        assert_eq!(
            ActivityTier::from_activity(15_000_000, 2_000_000),
            ActivityTier::Platinum
        );
        assert_eq!(
            ActivityTier::from_activity(5_000_000, 500_000),
            ActivityTier::Gold
        );
        assert_eq!(
            ActivityTier::from_activity(500_000, 50_000),
            ActivityTier::Silver
        );
        assert_eq!(
            ActivityTier::from_activity(10_000, 1_000),
            ActivityTier::Bronze
        );
    }

    #[test]
    fn test_tier_shares() {
        let total = 100u128;
        let platinum = total * ActivityTier::Platinum.pool_share_percent() as u128 / 100;
        let gold = total * ActivityTier::Gold.pool_share_percent() as u128 / 100;
        let silver = total * ActivityTier::Silver.pool_share_percent() as u128 / 100;
        let bronze = total * ActivityTier::Bronze.pool_share_percent() as u128 / 100;

        assert_eq!(platinum + gold + silver + bronze, 100);
    }

    #[test]
    fn test_federation_registration() {
        let mut rewards = FederationRewards::new();

        let metrics = ActivityMetrics {
            entity_id: [1u8; 32],
            monthly_transactions: 1_000_000,
            active_users_30d: 100_000,
            ..Default::default()
        };

        rewards.register_federation([1u8; 32], "Test Fed".to_string(), metrics);

        let fed = rewards.get_federation(&[1u8; 32]).unwrap();
        assert_eq!(fed.tier, ActivityTier::Gold);
        assert!(fed.is_eligible);
    }
}
