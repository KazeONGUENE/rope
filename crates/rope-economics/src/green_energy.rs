//! # Green Energy Verification
//!
//! Verification and bonuses for renewable energy usage.
//!
//! ## Energy Sources & Multipliers
//!
//! | Source | Verification | Multiplier |
//! |--------|--------------|------------|
//! | Solar | Certificate | 1.25x |
//! | Wind | Certificate | 1.25x |
//! | Hydro | Certificate | 1.20x |
//! | Nuclear | Certificate | 1.15x |
//! | Grid Mix | Default | 1.0x |
//! | High Carbon | Penalty | 0.85x |

use crate::constants::ONE_FAT;
use serde::{Deserialize, Serialize};

/// Energy source classification
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EnergySource {
    /// 100% solar power
    Solar,
    /// 100% wind power
    Wind,
    /// Hydroelectric power
    Hydro,
    /// Nuclear power
    Nuclear,
    /// Geothermal
    Geothermal,
    /// Mixed renewable sources
    RenewableMix,
    /// Standard grid (unknown mix)
    GridMix,
    /// High carbon (coal, gas)
    HighCarbon,
}

impl EnergySource {
    /// Get reward multiplier for energy source
    pub fn multiplier(&self) -> f64 {
        match self {
            Self::Solar | Self::Wind => 1.25,
            Self::Hydro | Self::Geothermal => 1.20,
            Self::Nuclear => 1.15,
            Self::RenewableMix => 1.15,
            Self::GridMix => 1.0,
            Self::HighCarbon => 0.85,
        }
    }

    /// Get carbon intensity (gCO2/kWh)
    pub fn carbon_intensity(&self) -> u32 {
        match self {
            Self::Solar => 40,
            Self::Wind => 10,
            Self::Hydro => 20,
            Self::Nuclear => 12,
            Self::Geothermal => 38,
            Self::RenewableMix => 50,
            Self::GridMix => 400,
            Self::HighCarbon => 900,
        }
    }

    /// Is this a renewable source?
    pub fn is_renewable(&self) -> bool {
        match self {
            Self::Solar | Self::Wind | Self::Hydro | Self::Geothermal | Self::RenewableMix => true,
            Self::Nuclear | Self::GridMix | Self::HighCarbon => false,
        }
    }

    /// Get name
    pub fn name(&self) -> &'static str {
        match self {
            Self::Solar => "Solar",
            Self::Wind => "Wind",
            Self::Hydro => "Hydroelectric",
            Self::Nuclear => "Nuclear",
            Self::Geothermal => "Geothermal",
            Self::RenewableMix => "Renewable Mix",
            Self::GridMix => "Grid Mix",
            Self::HighCarbon => "High Carbon",
        }
    }
}

/// Energy certificate verification
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EnergyCertificate {
    /// Certificate ID
    pub certificate_id: String,

    /// Issuing authority
    pub issuer: String,

    /// Energy source
    pub source: EnergySource,

    /// Verified percentage (0-100)
    pub verified_percent: u8,

    /// Valid from timestamp
    pub valid_from: i64,

    /// Valid until timestamp
    pub valid_until: i64,

    /// Monthly MWh covered
    pub monthly_mwh: u64,

    /// Verification hash
    pub verification_hash: [u8; 32],

    /// Is verified by network
    pub is_verified: bool,
}

impl EnergyCertificate {
    /// Check if certificate is currently valid
    pub fn is_valid(&self, timestamp: i64) -> bool {
        self.is_verified && timestamp >= self.valid_from && timestamp < self.valid_until
    }

    /// Calculate effective multiplier
    pub fn effective_multiplier(&self, timestamp: i64) -> f64 {
        if !self.is_valid(timestamp) {
            return 1.0;
        }

        // Blend multiplier based on verified percentage
        let source_multiplier = self.source.multiplier();
        let grid_multiplier = 1.0; // Default

        let verified_ratio = self.verified_percent as f64 / 100.0;
        source_multiplier * verified_ratio + grid_multiplier * (1.0 - verified_ratio)
    }
}

/// Green energy verification system
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GreenEnergyVerification {
    /// Node ID
    pub node_id: [u8; 32],

    /// Primary energy source
    pub primary_source: EnergySource,

    /// Certificates
    pub certificates: Vec<EnergyCertificate>,

    /// Total verified renewable percentage
    pub verified_renewable_percent: u8,

    /// Self-reported renewable percentage
    pub reported_renewable_percent: u8,

    /// Monthly energy consumption (kWh)
    pub monthly_consumption_kwh: u64,

    /// Monthly carbon footprint (kgCO2)
    pub monthly_carbon_kg: u64,

    /// Last verification timestamp
    pub last_verified: i64,
}

impl GreenEnergyVerification {
    /// Create new verification for a node
    pub fn new(node_id: [u8; 32], primary_source: EnergySource) -> Self {
        Self {
            node_id,
            primary_source,
            certificates: Vec::new(),
            verified_renewable_percent: 0,
            reported_renewable_percent: 0,
            monthly_consumption_kwh: 0,
            monthly_carbon_kg: 0,
            last_verified: 0,
        }
    }

    /// Add certificate
    pub fn add_certificate(&mut self, cert: EnergyCertificate) {
        self.certificates.push(cert);
        self.recalculate_verified_percent();
    }

    /// Recalculate verified percentage from valid certificates
    fn recalculate_verified_percent(&mut self) {
        let timestamp = chrono::Utc::now().timestamp();
        let valid_certs: Vec<_> = self
            .certificates
            .iter()
            .filter(|c| c.is_valid(timestamp) && c.source.is_renewable())
            .collect();

        if valid_certs.is_empty() {
            self.verified_renewable_percent = 0;
        } else {
            // Average of all valid certificate percentages
            let total: u32 = valid_certs.iter().map(|c| c.verified_percent as u32).sum();
            self.verified_renewable_percent = (total / valid_certs.len() as u32).min(100) as u8;
        }
    }

    /// Get effective reward multiplier
    pub fn reward_multiplier(&self, _timestamp: i64) -> f64 {
        if self.verified_renewable_percent >= 100 {
            self.primary_source.multiplier()
        } else if self.verified_renewable_percent >= 75 {
            1.20
        } else if self.verified_renewable_percent >= 50 {
            1.10
        } else if self.verified_renewable_percent >= 25 {
            1.05
        } else {
            1.0
        }
    }

    /// Calculate annual carbon offset bonus (in FAT)
    pub fn annual_carbon_bonus(&self) -> u128 {
        // Calculate avoided emissions compared to grid average
        let grid_carbon = EnergySource::GridMix.carbon_intensity() as u64;
        let actual_carbon = self.primary_source.carbon_intensity() as u64;

        if actual_carbon >= grid_carbon {
            return 0;
        }

        let avoided_per_kwh = grid_carbon - actual_carbon;
        let annual_kwh = self.monthly_consumption_kwh * 12;
        let avoided_kg = avoided_per_kwh * annual_kwh / 1000;

        // Bonus: 0.1 FAT per kg CO2 avoided
        avoided_kg as u128 * ONE_FAT / 10
    }
}

/// Green energy multiplier calculation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GreenEnergyMultiplier {
    /// Base multiplier from source
    pub source_multiplier: f64,

    /// Certificate bonus
    pub certificate_bonus: f64,

    /// Carbon offset bonus
    pub carbon_bonus: f64,

    /// Final multiplier
    pub final_multiplier: f64,
}

impl GreenEnergyMultiplier {
    /// Calculate from verification
    pub fn from_verification(verification: &GreenEnergyVerification, _timestamp: i64) -> Self {
        let source_multiplier = verification.primary_source.multiplier();

        // Certificate bonus based on verified percentage
        let certificate_bonus = if verification.verified_renewable_percent >= 100 {
            0.10
        } else if verification.verified_renewable_percent >= 75 {
            0.05
        } else if verification.verified_renewable_percent >= 50 {
            0.03
        } else {
            0.0
        };

        // Carbon offset bonus (capped at 0.05)
        let carbon_bonus = if verification.primary_source.is_renewable() {
            0.05
        } else {
            0.0
        };

        let final_multiplier = source_multiplier + certificate_bonus + carbon_bonus;

        Self {
            source_multiplier,
            certificate_bonus,
            carbon_bonus,
            final_multiplier,
        }
    }
}

/// Accepted certificate issuers
pub const ACCEPTED_ISSUERS: &[&str] = &[
    "I-REC Standard",
    "Guarantee of Origin (EU)",
    "REC (US)",
    "GreenPower (Australia)",
    "TIGR (Mexico)",
    "J-Credit (Japan)",
    "CER (Carbon Registry)",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_energy_multipliers() {
        assert_eq!(EnergySource::Solar.multiplier(), 1.25);
        assert_eq!(EnergySource::Wind.multiplier(), 1.25);
        assert_eq!(EnergySource::Nuclear.multiplier(), 1.15);
        assert_eq!(EnergySource::GridMix.multiplier(), 1.0);
        assert_eq!(EnergySource::HighCarbon.multiplier(), 0.85);
    }

    #[test]
    fn test_renewable_check() {
        assert!(EnergySource::Solar.is_renewable());
        assert!(EnergySource::Wind.is_renewable());
        assert!(EnergySource::Hydro.is_renewable());
        assert!(!EnergySource::Nuclear.is_renewable());
        assert!(!EnergySource::GridMix.is_renewable());
    }

    #[test]
    fn test_certificate_validity() {
        let cert = EnergyCertificate {
            certificate_id: "TEST-001".to_string(),
            issuer: "I-REC Standard".to_string(),
            source: EnergySource::Solar,
            verified_percent: 100,
            valid_from: 0,
            valid_until: 2_000_000_000,
            monthly_mwh: 100,
            verification_hash: [0u8; 32],
            is_verified: true,
        };

        assert!(cert.is_valid(1_000_000_000));
        assert!(!cert.is_valid(2_500_000_000));
    }

    #[test]
    fn test_verification_multiplier() {
        let mut verification = GreenEnergyVerification::new([0u8; 32], EnergySource::Solar);
        verification.verified_renewable_percent = 100;

        let multiplier = verification.reward_multiplier(0);
        assert_eq!(multiplier, 1.25);
    }
}
