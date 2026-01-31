//! # Performance Scoring
//!
//! Multi-metric performance evaluation for validators and node operators.
//!
//! ## Performance Metrics
//!
//! | Metric | Weight | Max Score | Description |
//! |--------|--------|-----------|-------------|
//! | Uptime | 30% | 0.60 | Node availability |
//! | Testimony Speed | 20% | 0.40 | Response latency |
//! | Bandwidth | 15% | 0.30 | Network contribution |
//! | Storage | 10% | 0.20 | Data storage |
//! | Green Energy | 10% | 0.20 | Renewable certification |
//! | Security | 10% | 0.20 | Infrastructure security |
//! | Geographic | 5% | 0.10 | Underserved regions |
//!
//! ## Multiplier Range
//!
//! - Maximum: 2.0x (all metrics perfect)
//! - Average: 1.0x (baseline)
//! - Minimum: 0.3x (poor performance)

use crate::constants::*;
use serde::{Deserialize, Serialize};

/// Performance metrics weights
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerformanceWeights {
    pub uptime: f64,
    pub testimony_speed: f64,
    pub bandwidth: f64,
    pub storage: f64,
    pub green_energy: f64,
    pub security: f64,
    pub geographic: f64,
}

impl Default for PerformanceWeights {
    fn default() -> Self {
        Self {
            uptime: 0.30,
            testimony_speed: 0.20,
            bandwidth: 0.15,
            storage: 0.10,
            green_energy: 0.10,
            security: 0.10,
            geographic: 0.05,
        }
    }
}

impl PerformanceWeights {
    /// Verify weights sum to 1.0
    pub fn verify(&self) -> bool {
        let sum = self.uptime
            + self.testimony_speed
            + self.bandwidth
            + self.storage
            + self.green_energy
            + self.security
            + self.geographic;
        (sum - 1.0).abs() < 0.001
    }
}

/// Raw performance metrics from node monitoring
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PerformanceMetrics {
    /// Uptime percentage (0-100)
    pub uptime_percent: f64,

    /// Average testimony response time in milliseconds
    pub testimony_latency_ms: u64,

    /// Bandwidth in Gbps
    pub bandwidth_gbps: f64,

    /// Storage in TB
    pub storage_tb: f64,

    /// Green energy percentage (0-100)
    pub green_energy_percent: u8,

    /// Security score (0-100)
    pub security_score: u8,

    /// Is in underserved geographic region
    pub is_underserved_region: bool,

    /// Number of testimonies provided this epoch
    pub testimonies_provided: u64,

    /// Users served this epoch
    pub users_served: u64,

    /// Strings stored
    pub strings_stored: u64,

    /// Measurement timestamp
    pub measured_at: i64,
}

/// Normalized performance scores (0.0 - 1.0 each)
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PerformanceScore {
    /// Uptime score (0-1)
    pub uptime: f64,

    /// Testimony speed score (0-1)
    pub testimony_speed: f64,

    /// Bandwidth score (0-1)
    pub bandwidth: f64,

    /// Storage score (0-1)
    pub storage: f64,

    /// Green energy score (0-1)
    pub green_energy: f64,

    /// Security score (0-1)
    pub security: f64,

    /// Geographic bonus score (0-1)
    pub geographic: f64,
}

impl PerformanceScore {
    /// Create score from raw metrics
    pub fn from_metrics(metrics: &PerformanceMetrics) -> Self {
        Self {
            uptime: Self::calculate_uptime_score(metrics.uptime_percent),
            testimony_speed: Self::calculate_testimony_score(metrics.testimony_latency_ms),
            bandwidth: Self::calculate_bandwidth_score(metrics.bandwidth_gbps),
            storage: Self::calculate_storage_score(metrics.storage_tb),
            green_energy: Self::calculate_green_energy_score(metrics.green_energy_percent),
            security: Self::calculate_security_score(metrics.security_score),
            geographic: if metrics.is_underserved_region {
                1.0
            } else {
                0.0
            },
        }
    }

    /// Calculate uptime score
    /// 99.9% = 1.0, 99% = 0.9, 98% = 0.7, 95% = 0.5, <90% = 0
    fn calculate_uptime_score(uptime_percent: f64) -> f64 {
        if uptime_percent >= 99.9 {
            1.0
        } else if uptime_percent >= 99.5 {
            0.95
        } else if uptime_percent >= 99.0 {
            0.9
        } else if uptime_percent >= 98.0 {
            0.7
        } else if uptime_percent >= 95.0 {
            0.5
        } else if uptime_percent >= 90.0 {
            0.3
        } else {
            0.0
        }
    }

    /// Calculate testimony speed score
    /// <100ms = 1.0, <200ms = 0.8, <500ms = 0.5, <1000ms = 0.3, >1000ms = 0.1
    fn calculate_testimony_score(latency_ms: u64) -> f64 {
        if latency_ms < 100 {
            1.0
        } else if latency_ms < 200 {
            0.8
        } else if latency_ms < 500 {
            0.5
        } else if latency_ms < 1000 {
            0.3
        } else {
            0.1
        }
    }

    /// Calculate bandwidth score
    /// 25+ Gbps = 1.0, 10 Gbps = 0.8, 1 Gbps = 0.5, 100 Mbps = 0.3
    fn calculate_bandwidth_score(bandwidth_gbps: f64) -> f64 {
        if bandwidth_gbps >= 25.0 {
            1.0
        } else if bandwidth_gbps >= 10.0 {
            0.8
        } else if bandwidth_gbps >= 1.0 {
            0.5
        } else if bandwidth_gbps >= 0.1 {
            0.3
        } else {
            0.1
        }
    }

    /// Calculate storage score
    /// 100+ TB = 1.0, 50 TB = 0.8, 10 TB = 0.5, 1 TB = 0.3
    fn calculate_storage_score(storage_tb: f64) -> f64 {
        if storage_tb >= 100.0 {
            1.0
        } else if storage_tb >= 50.0 {
            0.8
        } else if storage_tb >= 10.0 {
            0.5
        } else if storage_tb >= 1.0 {
            0.3
        } else {
            0.1
        }
    }

    /// Calculate green energy score
    /// 100% = 1.0, 75% = 0.75, 50% = 0.5, 25% = 0.25, 0% = 0
    fn calculate_green_energy_score(green_percent: u8) -> f64 {
        green_percent as f64 / 100.0
    }

    /// Calculate security score (0-100 scale to 0-1)
    fn calculate_security_score(score: u8) -> f64 {
        score as f64 / 100.0
    }

    /// Calculate weighted total score (0.0 - 1.0)
    pub fn weighted_total(&self) -> f64 {
        let weights = PerformanceWeights::default();

        self.uptime * weights.uptime
            + self.testimony_speed * weights.testimony_speed
            + self.bandwidth * weights.bandwidth
            + self.storage * weights.storage
            + self.green_energy * weights.green_energy
            + self.security * weights.security
            + self.geographic * weights.geographic
    }

    /// Calculate performance multiplier (0.3 - 2.0)
    pub fn multiplier(&self) -> f64 {
        let raw = self.weighted_total();
        // Scale from [0, 1] to [0.3, 2.0]
        MIN_PERFORMANCE_MULTIPLIER
            + (raw * (MAX_PERFORMANCE_MULTIPLIER - MIN_PERFORMANCE_MULTIPLIER))
    }
}

/// Performance multiplier with bonuses
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PerformanceMultiplier {
    /// Base multiplier from performance score
    pub base_multiplier: f64,

    /// Green energy bonus (up to 1.25x)
    pub green_energy_bonus: f64,

    /// Federation operator bonus (1.3x)
    pub federation_bonus: f64,

    /// Community operator bonus (1.2x)
    pub community_bonus: f64,

    /// Long-term commitment bonus (1.2x for >12 months)
    pub long_term_bonus: f64,

    /// Geographic diversity bonus (1.1x)
    pub geographic_bonus: f64,

    /// Hardware security bonus (1.15x)
    pub hardware_security_bonus: f64,

    /// Final combined multiplier
    pub final_multiplier: f64,
}

impl PerformanceMultiplier {
    /// Create from performance score and operator status
    pub fn calculate(
        score: &PerformanceScore,
        is_federation_operator: bool,
        is_community_operator: bool,
        months_active: u64,
        has_hardware_security: bool,
    ) -> Self {
        let base_multiplier = score.multiplier();

        // Green energy bonus: 100% = 1.25x, 75% = 1.15x, 50% = 1.10x
        let green_energy_bonus = if score.green_energy >= 1.0 {
            1.25
        } else if score.green_energy >= 0.75 {
            1.15
        } else if score.green_energy >= 0.50 {
            1.10
        } else if score.green_energy >= 0.25 {
            1.05
        } else {
            1.0
        };

        let federation_bonus = if is_federation_operator { 1.3 } else { 1.0 };
        let community_bonus = if is_community_operator && !is_federation_operator {
            1.2
        } else {
            1.0
        };
        let long_term_bonus = if months_active >= 12 {
            1.2
        } else if months_active >= 6 {
            1.1
        } else {
            1.0
        };
        let geographic_bonus = if score.geographic >= 0.5 { 1.1 } else { 1.0 };
        let hardware_security_bonus = if has_hardware_security { 1.15 } else { 1.0 };

        // Combine multipliers
        let final_multiplier = base_multiplier
            * green_energy_bonus
            * federation_bonus
            * community_bonus
            * long_term_bonus
            * geographic_bonus
            * hardware_security_bonus;

        Self {
            base_multiplier,
            green_energy_bonus,
            federation_bonus,
            community_bonus,
            long_term_bonus,
            geographic_bonus,
            hardware_security_bonus,
            final_multiplier,
        }
    }

    /// Create with just performance score (no bonuses)
    pub fn from_score(score: &PerformanceScore) -> Self {
        Self::calculate(score, false, false, 0, false)
    }
}

/// Performance tier classification
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PerformanceTier {
    /// Top 10% performers (multiplier > 1.8)
    Elite,
    /// Top 25% performers (multiplier > 1.5)
    Excellent,
    /// Top 50% performers (multiplier > 1.2)
    Good,
    /// Average performers (multiplier > 0.8)
    Standard,
    /// Below average (multiplier > 0.5)
    BelowAverage,
    /// Poor performers (multiplier <= 0.5)
    Poor,
}

impl PerformanceTier {
    /// Classify based on multiplier
    pub fn from_multiplier(multiplier: f64) -> Self {
        if multiplier >= 1.8 {
            Self::Elite
        } else if multiplier >= 1.5 {
            Self::Excellent
        } else if multiplier >= 1.2 {
            Self::Good
        } else if multiplier >= 0.8 {
            Self::Standard
        } else if multiplier >= 0.5 {
            Self::BelowAverage
        } else {
            Self::Poor
        }
    }

    /// Get tier name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Elite => "Elite",
            Self::Excellent => "Excellent",
            Self::Good => "Good",
            Self::Standard => "Standard",
            Self::BelowAverage => "Below Average",
            Self::Poor => "Poor",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_weights_sum_to_one() {
        let weights = PerformanceWeights::default();
        assert!(weights.verify());
    }

    #[test]
    fn test_perfect_score_multiplier() {
        let score = PerformanceScore {
            uptime: 1.0,
            testimony_speed: 1.0,
            bandwidth: 1.0,
            storage: 1.0,
            green_energy: 1.0,
            security: 1.0,
            geographic: 1.0,
        };

        let multiplier = score.multiplier();
        assert!((multiplier - 2.0).abs() < 0.01);
    }

    #[test]
    fn test_zero_score_multiplier() {
        let score = PerformanceScore::default();
        let multiplier = score.multiplier();
        assert!((multiplier - 0.3).abs() < 0.01);
    }

    #[test]
    fn test_average_score() {
        let score = PerformanceScore {
            uptime: 0.5,
            testimony_speed: 0.5,
            bandwidth: 0.5,
            storage: 0.5,
            green_energy: 0.5,
            security: 0.5,
            geographic: 0.5,
        };

        let total = score.weighted_total();
        assert!((total - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_federation_bonus() {
        let score = PerformanceScore {
            uptime: 1.0,
            testimony_speed: 1.0,
            bandwidth: 1.0,
            storage: 1.0,
            green_energy: 1.0,
            security: 1.0,
            geographic: 0.0,
        };

        let base = PerformanceMultiplier::from_score(&score);
        let with_federation = PerformanceMultiplier::calculate(&score, true, false, 0, false);

        assert!(with_federation.final_multiplier > base.final_multiplier);
        assert!((with_federation.federation_bonus - 1.3).abs() < 0.01);
    }

    #[test]
    fn test_uptime_scoring() {
        assert_eq!(PerformanceScore::calculate_uptime_score(99.95), 1.0);
        assert_eq!(PerformanceScore::calculate_uptime_score(99.5), 0.95);
        assert_eq!(PerformanceScore::calculate_uptime_score(99.0), 0.9);
        assert_eq!(PerformanceScore::calculate_uptime_score(85.0), 0.0);
    }
}
