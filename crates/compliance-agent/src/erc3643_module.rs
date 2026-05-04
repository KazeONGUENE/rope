// =============================================================================
// ERC-3643 Compliance Module for the Datachain Rope ComplianceAgent
// =============================================================================
//
// This module integrates the ERC-3643 (T-REX) compliance logic into the Rope
// node's ComplianceAgent AI Testimony system.  It:
//
//   1. Listens for `TestimonyRequested` events on the RopeComplianceModule
//      smart contract.
//   2. Performs off-chain MiFID II validation using configurable rules.
//   3. Writes the AI Testimony back on-chain via `recordExternalTestimony()`.
//
// Author: Kazé A. ONGUENE — Datachain Foundation
// =============================================================================

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

// =============================================================================
// Claim Topics (mirrors Solidity constants)
// =============================================================================

pub const KYC_VALIDATED: u64 = 1;
pub const AML_VALIDATED: u64 = 2;
pub const COUNTRY: u64 = 3;
pub const ACCREDITED_INVESTOR: u64 = 4;
pub const DCNFT_HOLDER: u64 = 10;
pub const SOVEREIGN_IDENTITY: u64 = 99;

// =============================================================================
// Configuration
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ERC3643ComplianceConfig {
    /// Maximum holders per ISO-3166 country code.
    pub max_holders_per_jurisdiction: HashMap<u16, u64>,

    /// ISO-3166 country codes that are completely restricted.
    pub restricted_countries: HashSet<u16>,

    /// Minimum investment amount in token smallest unit.
    pub min_investment_amount: u128,

    /// Whether both parties must hold ACCREDITED_INVESTOR claim.
    pub require_accredited_investor: bool,

    /// Lockup period in days after first mint.
    pub lockup_period_days: u32,

    /// On-chain RopeComplianceModule contract address.
    pub compliance_module_address: String,

    /// On-chain IdentityRegistry address.
    pub identity_registry_address: String,

    /// RPC endpoint for Datachain Rope.
    pub rpc_url: String,
}

impl Default for ERC3643ComplianceConfig {
    fn default() -> Self {
        Self {
            max_holders_per_jurisdiction: HashMap::new(),
            restricted_countries: HashSet::new(),
            min_investment_amount: 0,
            require_accredited_investor: false,
            lockup_period_days: 0,
            compliance_module_address: String::new(),
            identity_registry_address: String::new(),
            rpc_url: "https://erpc.datachain.network".to_string(),
        }
    }
}

// =============================================================================
// Transfer Validation Request
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransferValidationRequest {
    pub token_address: String,
    pub from: String,
    pub to: String,
    pub amount: u128,
    pub from_onchain_id: String,
    pub to_onchain_id: String,
    pub claims: Vec<OnchainClaim>,
    pub nonce: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnchainClaim {
    pub topic: u64,
    pub issuer: String,
    pub data: Vec<u8>,
    pub signature: Vec<u8>,
}

// =============================================================================
// Compliance Decision
// =============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceDecision {
    pub allowed: bool,
    pub reason: Option<String>,
    pub testimony_hash: [u8; 32],
    pub nonce: u64,
    pub checked_rules: Vec<String>,
    pub timestamp: u64,
}

// =============================================================================
// ERC-3643 Compliance Module
// =============================================================================

pub struct ERC3643ComplianceModule {
    config: ERC3643ComplianceConfig,
}

impl ERC3643ComplianceModule {
    pub fn new(config: ERC3643ComplianceConfig) -> Self {
        Self { config }
    }

    /// Primary entry point: validate a pending ERC-3643 transfer.
    pub fn validate_transfer(&self, request: &TransferValidationRequest) -> ComplianceDecision {
        let mut checked_rules = Vec::new();
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Rule 1 — Minimum investment
        checked_rules.push("min_investment".to_string());
        if request.amount < self.config.min_investment_amount {
            return self.deny(
                request.nonce,
                "Transfer amount below minimum investment threshold",
                checked_rules,
                timestamp,
            );
        }

        // Rule 2 — KYC & AML claims present for receiver
        checked_rules.push("kyc_aml_claims".to_string());
        let receiver_has_kyc = request.claims.iter().any(|c| c.topic == KYC_VALIDATED);
        let receiver_has_aml = request.claims.iter().any(|c| c.topic == AML_VALIDATED);

        if !receiver_has_kyc || !receiver_has_aml {
            return self.deny(
                request.nonce,
                "Receiver missing KYC or AML claims",
                checked_rules,
                timestamp,
            );
        }

        // Rule 3 — Country restriction
        checked_rules.push("country_restriction".to_string());
        if let Some(country_claim) = request.claims.iter().find(|c| c.topic == COUNTRY) {
            if country_claim.data.len() >= 2 {
                let country_code =
                    u16::from_be_bytes([country_claim.data[0], country_claim.data[1]]);
                if self.config.restricted_countries.contains(&country_code) {
                    return self.deny(
                        request.nonce,
                        &format!("Country {} is restricted", country_code),
                        checked_rules,
                        timestamp,
                    );
                }
            }
        }

        // Rule 4 — Accredited investor requirement
        if self.config.require_accredited_investor {
            checked_rules.push("accredited_investor".to_string());
            let has_accreditation = request
                .claims
                .iter()
                .any(|c| c.topic == ACCREDITED_INVESTOR);
            if !has_accreditation {
                return self.deny(
                    request.nonce,
                    "Accredited investor claim required but not found",
                    checked_rules,
                    timestamp,
                );
            }
        }

        // Rule 5 — Sovereign identity claim (Datawallet+ specific)
        checked_rules.push("sovereign_identity".to_string());
        let has_sovereign = request.claims.iter().any(|c| c.topic == SOVEREIGN_IDENTITY);
        if !has_sovereign {
            return self.deny(
                request.nonce,
                "Sovereign identity claim (Datawallet+) required",
                checked_rules,
                timestamp,
            );
        }

        // All rules passed
        self.approve(request.nonce, checked_rules, timestamp)
    }

    // =========================================================================
    // Helpers
    // =========================================================================

    fn approve(
        &self,
        nonce: u64,
        checked_rules: Vec<String>,
        timestamp: u64,
    ) -> ComplianceDecision {
        let hash = self.compute_testimony_hash(nonce, true, timestamp);
        ComplianceDecision {
            allowed: true,
            reason: None,
            testimony_hash: hash,
            nonce,
            checked_rules,
            timestamp,
        }
    }

    fn deny(
        &self,
        nonce: u64,
        reason: &str,
        checked_rules: Vec<String>,
        timestamp: u64,
    ) -> ComplianceDecision {
        let hash = self.compute_testimony_hash(nonce, false, timestamp);
        ComplianceDecision {
            allowed: false,
            reason: Some(reason.to_string()),
            testimony_hash: hash,
            nonce,
            checked_rules,
            timestamp,
        }
    }

    fn compute_testimony_hash(&self, nonce: u64, allowed: bool, timestamp: u64) -> [u8; 32] {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        nonce.hash(&mut hasher);
        allowed.hash(&mut hasher);
        timestamp.hash(&mut hasher);
        let h = hasher.finish();

        let mut result = [0u8; 32];
        result[..8].copy_from_slice(&h.to_be_bytes());
        // Pad with secondary hash for full 32 bytes
        (nonce.wrapping_mul(timestamp)).hash(&mut hasher);
        let h2 = hasher.finish();
        result[8..16].copy_from_slice(&h2.to_be_bytes());
        result[16..24].copy_from_slice(&h.to_le_bytes());
        result[24..32].copy_from_slice(&h2.to_le_bytes());
        result
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn default_config() -> ERC3643ComplianceConfig {
        ERC3643ComplianceConfig {
            min_investment_amount: 1000,
            require_accredited_investor: false,
            restricted_countries: {
                let mut set = HashSet::new();
                set.insert(408); // DPRK
                set.insert(364); // Iran
                set
            },
            ..Default::default()
        }
    }

    fn valid_claims() -> Vec<OnchainClaim> {
        vec![
            OnchainClaim {
                topic: KYC_VALIDATED,
                issuer: "0xDatawallet".to_string(),
                data: vec![],
                signature: vec![],
            },
            OnchainClaim {
                topic: AML_VALIDATED,
                issuer: "0xDatawallet".to_string(),
                data: vec![],
                signature: vec![],
            },
            OnchainClaim {
                topic: COUNTRY,
                issuer: "0xDatawallet".to_string(),
                data: vec![0, 250], // France = 250
                signature: vec![],
            },
            OnchainClaim {
                topic: SOVEREIGN_IDENTITY,
                issuer: "0xDatawallet".to_string(),
                data: vec![],
                signature: vec![],
            },
        ]
    }

    fn base_request() -> TransferValidationRequest {
        TransferValidationRequest {
            token_address: "0xToken".to_string(),
            from: "0xAlice".to_string(),
            to: "0xBob".to_string(),
            amount: 5000,
            from_onchain_id: "0xAliceID".to_string(),
            to_onchain_id: "0xBobID".to_string(),
            claims: valid_claims(),
            nonce: 1,
        }
    }

    #[test]
    fn test_valid_transfer_passes() {
        let module = ERC3643ComplianceModule::new(default_config());
        let decision = module.validate_transfer(&base_request());
        assert!(decision.allowed);
        assert!(decision.reason.is_none());
    }

    #[test]
    fn test_below_minimum_amount_rejected() {
        let module = ERC3643ComplianceModule::new(default_config());
        let mut req = base_request();
        req.amount = 500;
        let decision = module.validate_transfer(&req);
        assert!(!decision.allowed);
        assert!(decision.reason.unwrap().contains("minimum"));
    }

    #[test]
    fn test_missing_kyc_rejected() {
        let module = ERC3643ComplianceModule::new(default_config());
        let mut req = base_request();
        req.claims.retain(|c| c.topic != KYC_VALIDATED);
        let decision = module.validate_transfer(&req);
        assert!(!decision.allowed);
        assert!(decision.reason.unwrap().contains("KYC"));
    }

    #[test]
    fn test_restricted_country_rejected() {
        let module = ERC3643ComplianceModule::new(default_config());
        let mut req = base_request();
        for claim in &mut req.claims {
            if claim.topic == COUNTRY {
                claim.data = vec![1, 144]; // 400 = close to DPRK 408
                claim.data = 408u16.to_be_bytes().to_vec();
            }
        }
        let decision = module.validate_transfer(&req);
        assert!(!decision.allowed);
        assert!(decision.reason.unwrap().contains("restricted"));
    }

    #[test]
    fn test_missing_sovereign_identity_rejected() {
        let module = ERC3643ComplianceModule::new(default_config());
        let mut req = base_request();
        req.claims.retain(|c| c.topic != SOVEREIGN_IDENTITY);
        let decision = module.validate_transfer(&req);
        assert!(!decision.allowed);
        assert!(decision.reason.unwrap().contains("Sovereign"));
    }

    #[test]
    fn test_accredited_investor_required_and_missing() {
        let mut config = default_config();
        config.require_accredited_investor = true;
        let module = ERC3643ComplianceModule::new(config);
        let decision = module.validate_transfer(&base_request());
        assert!(!decision.allowed);
        assert!(decision.reason.unwrap().contains("Accredited"));
    }

    #[test]
    fn test_accredited_investor_required_and_present() {
        let mut config = default_config();
        config.require_accredited_investor = true;
        let module = ERC3643ComplianceModule::new(config);
        let mut req = base_request();
        req.claims.push(OnchainClaim {
            topic: ACCREDITED_INVESTOR,
            issuer: "0xDatawallet".to_string(),
            data: vec![],
            signature: vec![],
        });
        let decision = module.validate_transfer(&req);
        assert!(decision.allowed);
    }
}
