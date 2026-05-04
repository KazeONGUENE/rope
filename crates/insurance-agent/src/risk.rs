//! Parametric risk model.
//!
//! ## Honest scope
//!
//! This is a **parametric formula**, not an actuarial model:
//!
//! ```text
//!   premium_bps = base_bps(asset_type) * jurisdiction_mult(location)
//!                                      * verified_mult(is_verified)
//!   premium_usd = valuation_usd * premium_bps / 10_000
//!   coverage_usd = valuation_usd * coverage_ratio(asset_type)
//! ```
//!
//! Triggers (the events that would pay the policy out) are picked from a
//! per-asset-type table. The output is a deterministic [`RiskProfile`] that
//! the [`crate::attestation`] module turns into a signed
//! [`crate::ParametricInsuranceAttestation`].
//!
//! No machine learning. No "AI underwriting". Just numbers any auditor can
//! reproduce in a spreadsheet.

use crate::attestation::TriggerCondition;
use crate::feeds::TokenizedAsset;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Output of one risk evaluation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskProfile {
    pub asset_id: String,
    pub asset_type: String,

    /// Annualised premium in USD.
    pub premium_usd: f64,

    /// Annualised premium in basis points of valuation.
    pub premium_bps: u32,

    /// Coverage cap in USD (paid out if any trigger fires).
    pub coverage_usd: f64,

    /// Triggers that would activate a payout under this profile.
    pub triggers: Vec<TriggerCondition>,

    /// Human-readable jurisdiction string used to derive the multiplier
    /// (echoed back so the attestation is auditable).
    pub jurisdiction: Option<String>,

    /// Multiplier actually applied (jurisdiction × verified). Useful for
    /// debugging and audits.
    pub effective_multiplier: f64,
}

/// Risk model knobs. The defaults bake in a defensible, conservative
/// rate-card; operators are expected to tune per-region multipliers in
/// production.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskModelConfig {
    /// Per-asset-type base premium in basis points (1 bp = 0.01%).
    pub base_premium_bps: HashMap<String, u32>,

    /// Per-asset-type coverage ratio (fraction of valuation covered).
    pub coverage_ratio: HashMap<String, f64>,

    /// Per-asset-type trigger templates.
    pub triggers: HashMap<String, Vec<TriggerCondition>>,

    /// Substring match → multiplier on jurisdiction. Lowercased.
    pub jurisdiction_multipliers: Vec<(String, f64)>,

    /// Default jurisdiction multiplier when no rule matches.
    pub default_jurisdiction_multiplier: f64,

    /// Multiplier applied when `is_verified == false` (riskier).
    pub unverified_multiplier: f64,

    /// Default base bps when the asset_type is not in the table.
    pub default_base_bps: u32,

    /// Default coverage ratio when the asset_type is not in the table.
    pub default_coverage_ratio: f64,
}

impl Default for RiskModelConfig {
    fn default() -> Self {
        // Base rate-card. Numbers reflect rough industry parametric ranges
        // for each asset class; they are bounded so an obviously-wrong
        // valuation cannot produce an obviously-wrong premium.
        let base_premium_bps = [
            ("GOLD_MINE", 280), // 2.8% — operational + sovereign risk
            ("RARE_EARTH", 320),
            ("OIL_FIELD", 350),
            ("DIAMOND", 220),
            ("FORESTRY", 180),   // 1.8% — wildfire + illegal logging
            ("REAL_ESTATE", 90), // 0.9% — base property cover
            ("AGRICULTURAL", 240),
            ("LUXURY_VEHICLE", 160),
            ("WATCH", 130),
            ("INFRASTRUCTURE", 200),
            ("CULTURAL_HERITAGE", 250),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), *v as u32))
        .collect();

        let coverage_ratio = [
            ("GOLD_MINE", 0.70),
            ("RARE_EARTH", 0.65),
            ("OIL_FIELD", 0.60),
            ("DIAMOND", 0.85),
            ("FORESTRY", 0.50),
            ("REAL_ESTATE", 0.80),
            ("AGRICULTURAL", 0.60),
            ("LUXURY_VEHICLE", 0.90),
            ("WATCH", 0.95),
            ("INFRASTRUCTURE", 0.55),
            ("CULTURAL_HERITAGE", 0.75),
        ]
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect();

        let triggers: HashMap<String, Vec<TriggerCondition>> = [
            (
                "GOLD_MINE",
                vec![
                    TriggerCondition::new(
                        "production_halt_30d",
                        "Daily extraction below 20% of baseline for 30+ days",
                    ),
                    TriggerCondition::new(
                        "force_majeure",
                        "Officially declared force majeure (war, civil unrest)",
                    ),
                    TriggerCondition::new(
                        "title_invalidation",
                        "DCNFT title deed marked invalid by ONCHAINID claim revocation",
                    ),
                ],
            ),
            (
                "RARE_EARTH",
                vec![
                    TriggerCondition::new(
                        "export_ban",
                        "Host nation imposes export ban for 60+ days",
                    ),
                    TriggerCondition::new(
                        "ore_grade_collapse",
                        "Verified ore grade falls > 40% vs. on-chain attestation",
                    ),
                ],
            ),
            (
                "OIL_FIELD",
                vec![
                    TriggerCondition::new(
                        "production_halt_30d",
                        "Daily output below 20% of baseline for 30+ days",
                    ),
                    TriggerCondition::new(
                        "spill_event",
                        "EPA/EU-equivalent declared spill on covered acreage",
                    ),
                ],
            ),
            (
                "DIAMOND",
                vec![
                    TriggerCondition::new(
                        "kimberley_decertification",
                        "Loss of Kimberley Process certification",
                    ),
                    TriggerCondition::new(
                        "vault_breach",
                        "Audited vault breach affecting > 5% of holdings",
                    ),
                ],
            ),
            (
                "FORESTRY",
                vec![
                    TriggerCondition::new(
                        "wildfire_loss_15pct",
                        "Satellite-confirmed canopy loss > 15%",
                    ),
                    TriggerCondition::new(
                        "illegal_logging_event",
                        "Verified illegal logging affecting > 5% of plot",
                    ),
                ],
            ),
            (
                "REAL_ESTATE",
                vec![
                    TriggerCondition::new(
                        "structural_loss",
                        "Independent structural assessment shows > 30% loss of value",
                    ),
                    TriggerCondition::new(
                        "natural_disaster",
                        "Government-declared natural disaster covering site coordinates",
                    ),
                ],
            ),
            (
                "AGRICULTURAL",
                vec![
                    TriggerCondition::new(
                        "drought_index",
                        "Standardised Precipitation Index < -1.5 for 90+ days",
                    ),
                    TriggerCondition::new(
                        "yield_loss_30pct",
                        "Verified yield loss > 30% vs. five-year mean",
                    ),
                ],
            ),
            (
                "LUXURY_VEHICLE",
                vec![
                    TriggerCondition::new(
                        "theft",
                        "Police-filed theft report referencing the DCNFT serial",
                    ),
                    TriggerCondition::new(
                        "total_loss",
                        "Independent appraisal flags vehicle as total loss",
                    ),
                ],
            ),
            (
                "WATCH",
                vec![TriggerCondition::new(
                    "theft_or_loss",
                    "Police-filed theft or loss report referencing DCNFT serial",
                )],
            ),
            (
                "INFRASTRUCTURE",
                vec![
                    TriggerCondition::new(
                        "service_outage_72h",
                        "Verified outage of the underlying service > 72h",
                    ),
                    TriggerCondition::new(
                        "regulatory_decommission",
                        "Regulator orders decommissioning of the asset",
                    ),
                ],
            ),
            (
                "CULTURAL_HERITAGE",
                vec![
                    TriggerCondition::new(
                        "looting_or_damage",
                        "Verified looting or damage reported by host institution",
                    ),
                    TriggerCondition::new("unesco_status_loss", "Loss of UNESCO recognised status"),
                ],
            ),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v))
        .collect();

        // Jurisdiction multiplier — substring matching, lowercased.
        // Higher means riskier means higher premium.
        let jurisdiction_multipliers = vec![
            ("congo".into(), 1.45),
            ("drc".into(), 1.45),
            ("sudan".into(), 1.60),
            ("yemen".into(), 1.65),
            ("somalia".into(), 1.65),
            ("haiti".into(), 1.40),
            ("venezuela".into(), 1.40),
            ("brazil".into(), 1.10),
            ("indonesia".into(), 1.15),
            ("nigeria".into(), 1.30),
            ("colombia".into(), 1.20),
            ("peru".into(), 1.10),
            ("united kingdom".into(), 0.85),
            ("germany".into(), 0.80),
            ("france".into(), 0.85),
            ("switzerland".into(), 0.75),
            ("united states".into(), 0.90),
            ("japan".into(), 0.80),
            ("singapore".into(), 0.78),
            ("united arab emirates".into(), 0.95),
        ];

        Self {
            base_premium_bps,
            coverage_ratio,
            triggers,
            jurisdiction_multipliers,
            default_jurisdiction_multiplier: 1.00,
            unverified_multiplier: 1.50,
            default_base_bps: 200,
            default_coverage_ratio: 0.50,
        }
    }
}

/// Stateless evaluator wrapped around a [`RiskModelConfig`].
#[derive(Debug, Clone)]
pub struct RiskModel {
    cfg: RiskModelConfig,
}

impl RiskModel {
    pub fn new(cfg: RiskModelConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &RiskModelConfig {
        &self.cfg
    }

    /// Evaluate a single asset and return a profile.
    pub fn evaluate(&self, asset: &TokenizedAsset) -> RiskProfile {
        let base_bps = *self
            .cfg
            .base_premium_bps
            .get(&asset.asset_type)
            .unwrap_or(&self.cfg.default_base_bps);

        let coverage_ratio = *self
            .cfg
            .coverage_ratio
            .get(&asset.asset_type)
            .unwrap_or(&self.cfg.default_coverage_ratio);

        let jurisdiction_mult = self.jurisdiction_multiplier(asset.location.as_deref());
        let verified_mult = if asset.is_verified {
            1.0
        } else {
            self.cfg.unverified_multiplier
        };
        let effective_multiplier = jurisdiction_mult * verified_mult;

        let effective_bps = ((base_bps as f64) * effective_multiplier).round() as u32;
        // Clamp at 100% — no parametric quote can exceed valuation.
        let effective_bps = effective_bps.min(10_000);

        let valuation = asset.valuation_usd.max(0.0);
        let premium_usd = (valuation * (effective_bps as f64) / 10_000.0).round();
        let coverage_usd = (valuation * coverage_ratio).round();

        let triggers = self
            .cfg
            .triggers
            .get(&asset.asset_type)
            .cloned()
            .unwrap_or_else(|| {
                vec![TriggerCondition::new(
                    "asset_status_revoked",
                    "DCNFT title or ERC-3643 token verification claim revoked on-chain",
                )]
            });

        RiskProfile {
            asset_id: asset.asset_id.clone(),
            asset_type: asset.asset_type.clone(),
            premium_usd,
            premium_bps: effective_bps,
            coverage_usd,
            triggers,
            jurisdiction: asset.location.clone(),
            effective_multiplier,
        }
    }

    fn jurisdiction_multiplier(&self, location: Option<&str>) -> f64 {
        let Some(loc) = location else {
            return self.cfg.default_jurisdiction_multiplier;
        };
        let needle = loc.to_lowercase();
        for (substr, mult) in &self.cfg.jurisdiction_multipliers {
            if needle.contains(substr) {
                return *mult;
            }
        }
        self.cfg.default_jurisdiction_multiplier
    }
}

impl Default for RiskModel {
    fn default() -> Self {
        Self::new(RiskModelConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::{AssetSource, TokenizedAsset};

    fn asset(
        asset_type: &str,
        location: Option<&str>,
        valuation: f64,
        verified: bool,
    ) -> TokenizedAsset {
        TokenizedAsset {
            asset_id: format!("test-{asset_type}"),
            name: format!("Test {asset_type}"),
            asset_type: asset_type.to_string(),
            location: location.map(|s| s.to_string()),
            valuation_usd: valuation,
            is_verified: verified,
            chain_id: Some(271828),
            dcnft_addr: Some("0xdcnft".into()),
            erc3643_addr: Some("0xerc3643".into()),
            source: AssetSource::Tanastok,
        }
    }

    #[test]
    fn gold_mine_in_drc_uses_country_multiplier() {
        let model = RiskModel::default();
        let p = model.evaluate(&asset(
            "GOLD_MINE",
            Some("Democratic Republic of Congo"),
            10_000_000.0,
            true,
        ));
        assert_eq!(p.asset_type, "GOLD_MINE");
        // 280 bps base * 1.45 (DRC) * 1.0 (verified) = 406 bps
        assert_eq!(p.premium_bps, 406);
        assert_eq!(p.premium_usd, 406_000.0);
        // 70% coverage
        assert_eq!(p.coverage_usd, 7_000_000.0);
        assert!(!p.triggers.is_empty());
        assert!(p.triggers.iter().any(|t| t.code == "production_halt_30d"));
    }

    #[test]
    fn forestry_in_brazil_uses_brazil_multiplier() {
        let model = RiskModel::default();
        let p = model.evaluate(&asset("FORESTRY", Some("Brazil"), 1_000_000.0, true));
        // 180 bps * 1.10 (Brazil) = 198 bps
        assert_eq!(p.premium_bps, 198);
        assert_eq!(p.premium_usd, 19_800.0);
        assert_eq!(p.coverage_usd, 500_000.0);
        assert!(p.triggers.iter().any(|t| t.code == "wildfire_loss_15pct"));
    }

    #[test]
    fn real_estate_uses_default_multiplier_when_country_unknown() {
        let model = RiskModel::default();
        let p = model.evaluate(&asset("REAL_ESTATE", Some("Atlantis"), 500_000.0, true));
        // 90 bps * 1.0 (default) = 90 bps
        assert_eq!(p.premium_bps, 90);
        assert_eq!(p.premium_usd, 4_500.0);
        // 80% coverage
        assert_eq!(p.coverage_usd, 400_000.0);
    }

    #[test]
    fn unverified_asset_pays_more() {
        let model = RiskModel::default();
        let verified = model.evaluate(&asset("REAL_ESTATE", Some("France"), 1_000_000.0, true));
        let unverified = model.evaluate(&asset("REAL_ESTATE", Some("France"), 1_000_000.0, false));
        assert!(unverified.premium_bps > verified.premium_bps);
        // 90 * 0.85 (FR) = 76.5 → 77, vs * 1.5 = 114.75 → 115
        assert_eq!(verified.premium_bps, 77);
        assert_eq!(unverified.premium_bps, 115);
    }

    #[test]
    fn unknown_asset_type_falls_back_to_defaults() {
        let model = RiskModel::default();
        let p = model.evaluate(&asset("MARTIAN_REGOLITH", None, 100_000.0, true));
        // default 200 bps * 1.0 = 200 bps
        assert_eq!(p.premium_bps, 200);
        assert_eq!(p.premium_usd, 2_000.0);
        // default 50% coverage
        assert_eq!(p.coverage_usd, 50_000.0);
        // fallback trigger
        assert_eq!(p.triggers.len(), 1);
        assert_eq!(p.triggers[0].code, "asset_status_revoked");
    }

    #[test]
    fn premium_clamped_at_full_valuation() {
        // Build a config with absurd numbers to verify the clamp.
        let mut cfg = RiskModelConfig::default();
        cfg.base_premium_bps.insert("EXTREME".to_string(), 12_000);
        cfg.unverified_multiplier = 5.0;
        let model = RiskModel::new(cfg);
        let p = model.evaluate(&asset("EXTREME", None, 1_000.0, false));
        assert_eq!(p.premium_bps, 10_000); // clamped
        assert_eq!(p.premium_usd, 1_000.0);
    }

    #[test]
    fn negative_valuation_treated_as_zero() {
        let model = RiskModel::default();
        let p = model.evaluate(&asset("GOLD_MINE", None, -100.0, true));
        assert_eq!(p.premium_usd, 0.0);
        assert_eq!(p.coverage_usd, 0.0);
    }
}
