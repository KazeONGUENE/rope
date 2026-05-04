//! `ParametricInsuranceAttestation` — what the InsuranceAgent emits.
//!
//! Each attestation is a JSON object that:
//!
//! 1. Identifies the asset (Tanastok ID + DCNFT + ERC-3643 contracts).
//! 2. Quotes premium and coverage in USD.
//! 3. Lists the parametric triggers that would fire a payout.
//! 4. Carries a validity window (`valid_from`, `valid_until`).
//! 5. Carries the `agent_id` so an explorer can attribute it.
//!
//! When the agent anchors it, the JSON above is wrapped in
//! `rope_appendToLedger`'s `interaction.metadata` field, with
//! `interaction_type = "ParametricInsuranceAttestation"`.

use crate::feeds::{AssetSource, TokenizedAsset};
use crate::risk::RiskProfile;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A single parametric trigger condition baked into an attestation.
///
/// `code` is a stable, machine-readable identifier (e.g. `wildfire_loss_15pct`).
/// `description` is a human-readable sentence the operator and any auditor
/// can read at a glance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriggerCondition {
    pub code: String,
    pub description: String,
}

impl TriggerCondition {
    pub fn new(code: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            description: description.into(),
        }
    }
}

/// Full attestation payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParametricInsuranceAttestation {
    pub asset_id: String,
    pub asset_type: String,
    pub source: AssetSource,

    pub dcnft_addr: Option<String>,
    pub erc3643_addr: Option<String>,
    pub chain_id: Option<u64>,

    pub valuation_usd: f64,
    pub premium_usd: f64,
    pub premium_bps: u32,
    pub coverage_usd: f64,
    pub jurisdiction: Option<String>,
    pub effective_multiplier: f64,

    pub triggers: Vec<TriggerCondition>,

    pub valid_from: i64,
    pub valid_until: i64,

    pub agent_id: String,

    /// Crate version that issued the attestation, for traceability.
    pub agent_version: String,
}

impl ParametricInsuranceAttestation {
    /// Build an attestation from an asset + risk profile + window.
    pub fn build(
        asset: &TokenizedAsset,
        profile: &RiskProfile,
        agent_id: impl Into<String>,
        valid_from: i64,
        valid_until: i64,
    ) -> Result<Self, AttestationError> {
        if valid_until <= valid_from {
            return Err(AttestationError::InvalidWindow {
                from: valid_from,
                until: valid_until,
            });
        }
        if asset.asset_id != profile.asset_id {
            return Err(AttestationError::AssetMismatch {
                asset: asset.asset_id.clone(),
                profile: profile.asset_id.clone(),
            });
        }

        Ok(Self {
            asset_id: asset.asset_id.clone(),
            asset_type: asset.asset_type.clone(),
            source: asset.source.clone(),
            dcnft_addr: asset.dcnft_addr.clone(),
            erc3643_addr: asset.erc3643_addr.clone(),
            chain_id: asset.chain_id,
            valuation_usd: asset.valuation_usd,
            premium_usd: profile.premium_usd,
            premium_bps: profile.premium_bps,
            coverage_usd: profile.coverage_usd,
            jurisdiction: profile.jurisdiction.clone(),
            effective_multiplier: profile.effective_multiplier,
            triggers: profile.triggers.clone(),
            valid_from,
            valid_until,
            agent_id: agent_id.into(),
            agent_version: crate::VERSION.to_string(),
        })
    }

    /// Compute a deterministic 32-byte digest over the canonicalised JSON.
    /// Used as a stable de-dup key and as the on-chain reference hash.
    pub fn digest(&self) -> AttestationDigest {
        let canonical = serde_json::to_vec(self).expect("serializing an attestation never fails");
        AttestationDigest(*blake3::hash(&canonical).as_bytes())
    }
}

/// 32-byte BLAKE3 digest of a canonical attestation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AttestationDigest(#[serde(with = "digest_bytes")] pub [u8; 32]);

impl AttestationDigest {
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }
}

mod digest_bytes {
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8; 32], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&hex::encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 32], D::Error> {
        let s = String::deserialize(deserializer)?;
        let v = hex::decode(s).map_err(serde::de::Error::custom)?;
        let mut out = [0u8; 32];
        if v.len() != 32 {
            return Err(serde::de::Error::custom("expected 32-byte hex digest"));
        }
        out.copy_from_slice(&v);
        Ok(out)
    }
}

#[derive(Debug, Error)]
pub enum AttestationError {
    #[error("invalid validity window: from={from} until={until}")]
    InvalidWindow { from: i64, until: i64 },

    #[error("asset and risk profile do not match: asset={asset} profile={profile}")]
    AssetMismatch { asset: String, profile: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feeds::AssetSource;
    use crate::risk::RiskModel;

    fn sample_asset() -> TokenizedAsset {
        TokenizedAsset {
            asset_id: "featured-kibali-gold-mine".into(),
            name: "Kibali Gold Mine, Congo DRC".into(),
            asset_type: "GOLD_MINE".into(),
            location: Some("Democratic Republic of Congo".into()),
            valuation_usd: 10_000_000_053.0,
            is_verified: true,
            chain_id: Some(271828),
            dcnft_addr: Some("0x91f884D436858ad221436573BC2cB5117E27e564".into()),
            erc3643_addr: Some("0x2D16be771cB30AEedD9913b70b6237a832828bbB".into()),
            source: AssetSource::Tanastok,
        }
    }

    #[test]
    fn build_attestation_for_gold_mine() {
        let model = RiskModel::default();
        let asset = sample_asset();
        let profile = model.evaluate(&asset);
        let att = ParametricInsuranceAttestation::build(
            &asset,
            &profile,
            "InsuranceAgent",
            1_700_000_000,
            1_700_000_000 + 7 * 86_400,
        )
        .unwrap();

        assert_eq!(att.asset_id, "featured-kibali-gold-mine");
        assert_eq!(att.asset_type, "GOLD_MINE");
        assert_eq!(
            att.dcnft_addr.as_deref(),
            Some("0x91f884D436858ad221436573BC2cB5117E27e564")
        );
        assert_eq!(
            att.erc3643_addr.as_deref(),
            Some("0x2D16be771cB30AEedD9913b70b6237a832828bbB")
        );
        assert_eq!(att.premium_bps, 406);
        assert!(att.premium_usd > 0.0);
        assert!(att.coverage_usd > 0.0);
        assert_eq!(att.agent_id, "InsuranceAgent");
        assert_eq!(att.valid_until - att.valid_from, 7 * 86_400);
    }

    #[test]
    fn rejects_inverted_validity_window() {
        let model = RiskModel::default();
        let asset = sample_asset();
        let profile = model.evaluate(&asset);
        let err = ParametricInsuranceAttestation::build(&asset, &profile, "InsuranceAgent", 10, 5)
            .unwrap_err();
        match err {
            AttestationError::InvalidWindow { from: 10, until: 5 } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn rejects_asset_profile_mismatch() {
        let model = RiskModel::default();
        let mut profile = model.evaluate(&sample_asset());
        profile.asset_id = "different-asset".into();
        let err = ParametricInsuranceAttestation::build(
            &sample_asset(),
            &profile,
            "InsuranceAgent",
            1,
            2,
        )
        .unwrap_err();
        match err {
            AttestationError::AssetMismatch { .. } => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn digest_is_stable_and_changes_with_payload() {
        let model = RiskModel::default();
        let asset = sample_asset();
        let profile = model.evaluate(&asset);

        let a =
            ParametricInsuranceAttestation::build(&asset, &profile, "Agent", 1, 1_000_001).unwrap();
        let b = a.clone();
        assert_eq!(a.digest(), b.digest(), "same payload → same digest");

        let mut c = a.clone();
        c.premium_usd += 1.0;
        assert_ne!(
            a.digest(),
            c.digest(),
            "different payload → different digest"
        );
    }
}
