//! AccessGrant engine - spec v2.0 §5.
//!
//! Every grant of external access is an explicit object with scope,
//! grantee, duration, price, and delivery method. API keys are minted FROM
//! grants and inherit their exact terms; revoking the grant kills every key
//! instantly. Grants touching regulators or the public carry a Timelock
//! delay so a change in public-facing policy is visible before it takes
//! effect.

use serde::{Deserialize, Serialize};

use crate::types::now_ts;

/// The five stakeholder classes from spec v1.0 §6.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StakeholderClass {
    Regulator,
    Government,
    Investor,
    Public,
    CommercialBuyer,
}

impl StakeholderClass {
    /// Regulator- and public-facing grants go through the Timelock window.
    pub fn timelocked(&self) -> bool {
        matches!(self, StakeholderClass::Regulator | StakeholderClass::Public)
    }
}

/// Who the grant is issued to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Grantee {
    /// `wallet`, `did`, `claim_class`, or `public`.
    pub kind: String,
    /// The wallet address, DID, ONCHAINID claim class name, or `*`.
    pub value: String,
}

/// Which slice of the project the grant covers.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GrantScope {
    /// Facets covered: assets, sensors, readings, diagnoses, approvals, external.
    #[serde(default)]
    pub facets: Vec<String>,
    /// Restrict to specific asset ids. Empty = all assets.
    #[serde(default)]
    pub asset_ids: Vec<String>,
    /// Restrict to asset categories. Empty = all categories.
    #[serde(default)]
    pub categories: Vec<String>,
}

impl GrantScope {
    pub fn allows_facet(&self, facet: &str) -> bool {
        self.facets.is_empty() || self.facets.iter().any(|f| f == facet)
    }

    pub fn allows_asset(&self, asset_id: &str, category: &str) -> bool {
        let asset_ok =
            self.asset_ids.is_empty() || self.asset_ids.iter().any(|a| a == asset_id);
        let cat_ok =
            self.categories.is_empty() || self.categories.iter().any(|c| c == category);
        asset_ok && cat_ok
    }
}

/// Pricing model for the grant (spec v1.0 §6.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GrantPrice {
    /// `free`, `one_time`, `subscription`, or `metered`.
    pub model: String,
    #[serde(default)]
    pub amount: f64,
    /// `FAT`, the project token symbol, or an ISO currency for fiat.
    #[serde(default)]
    pub currency: String,
    /// For subscriptions: `monthly`, `quarterly`, `yearly`.
    #[serde(default)]
    pub period: String,
}

impl Default for GrantPrice {
    fn default() -> Self {
        Self {
            model: "free".to_string(),
            amount: 0.0,
            currency: "FAT".to_string(),
            period: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantStatus {
    PendingTimelock,
    Active,
    Revoked,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessGrant {
    pub id: String,
    pub project_id: String,
    pub grantee: Grantee,
    pub stakeholder_class: StakeholderClass,
    pub scope: GrantScope,
    pub starts_at: i64,
    /// 0 = indefinite until revoked.
    pub expires_at: i64,
    pub price: GrantPrice,
    /// Delivery methods: `rest`, `stream`, `export`.
    pub delivery: Vec<String>,
    pub status: GrantStatus,
    /// When the grant becomes usable (Timelock ETA for regulator/public grants).
    pub effective_at: i64,
    pub created_by: String,
    pub created_at: i64,
    #[serde(default)]
    pub revoked_at: i64,
    /// Knot hash of the `AccessGrantIssued` anchor.
    #[serde(default)]
    pub anchor_knot: String,
    /// Metering counters (spec v2.0 §5.3).
    #[serde(default)]
    pub calls: u64,
    #[serde(default)]
    pub last_used_at: i64,
    /// Scheduled bulk-export cadence (spec v1.0 §6.3): `""` (off),
    /// `hourly`, `daily`, or `weekly`. Only meaningful when `delivery`
    /// includes `export`.
    #[serde(default)]
    pub export_schedule: String,
    /// When the last scheduled export was produced (unix seconds).
    #[serde(default)]
    pub last_export_at: i64,
    /// Calls already invoiced on a closed billing statement - the
    /// metered-billing window is `calls - billed_calls`.
    #[serde(default)]
    pub billed_calls: u64,
    /// When the last billing statement was closed (unix seconds).
    #[serde(default)]
    pub last_billed_at: i64,
}

impl AccessGrant {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project_id: &str,
        grantee: Grantee,
        stakeholder_class: StakeholderClass,
        scope: GrantScope,
        starts_at: i64,
        expires_at: i64,
        price: GrantPrice,
        delivery: Vec<String>,
        created_by: &str,
        timelock_delay_secs: i64,
    ) -> Self {
        let uid = uuid::Uuid::new_v4().simple().to_string();
        let now = now_ts();
        let (status, effective_at) = if stakeholder_class.timelocked() {
            (GrantStatus::PendingTimelock, now + timelock_delay_secs)
        } else {
            (GrantStatus::Active, now)
        };
        Self {
            id: format!("gr_{}", &uid[..12]),
            project_id: project_id.to_string(),
            grantee,
            stakeholder_class,
            scope,
            starts_at,
            expires_at,
            price,
            delivery,
            status,
            effective_at,
            created_by: created_by.to_string(),
            created_at: now,
            revoked_at: 0,
            anchor_knot: String::new(),
            calls: 0,
            last_used_at: 0,
            export_schedule: String::new(),
            last_export_at: 0,
            billed_calls: 0,
            last_billed_at: 0,
        }
    }

    /// Whether the grant authorizes anything right now. Promotes
    /// `PendingTimelock` → `Active` once the ETA passed, and enforces
    /// start/expiry regardless of the stored status field.
    pub fn is_usable(&self, now: i64) -> bool {
        if matches!(self.status, GrantStatus::Revoked) {
            return false;
        }
        if now < self.effective_at || now < self.starts_at {
            return false;
        }
        if self.expires_at > 0 && now >= self.expires_at {
            return false;
        }
        true
    }

    pub fn allows_delivery(&self, method: &str) -> bool {
        self.delivery.is_empty() || self.delivery.iter().any(|d| d == method)
    }
}

/// An API key minted from a grant. Only the blake3 digest of the bearer
/// token is stored (spec v2.0 §5.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKeyRecord {
    pub id: String,
    pub grant_id: String,
    /// hex(blake3(token)) - the plaintext token is returned exactly once.
    pub token_digest: String,
    pub created_at: i64,
    #[serde(default)]
    pub revoked: bool,
    #[serde(default)]
    pub label: String,
    /// Sandbox key (spec v1.0 §6.3): served from synthetic data derived
    /// from the project's own sensor declarations - never from the live
    /// stream. Lets a stakeholder validate their integration first.
    #[serde(default)]
    pub sandbox: bool,
}

/// Mint a bearer token for a grant. Returns `(record, plaintext_token)`.
/// The plaintext is shown to the caller once and never persisted.
/// Sandbox keys carry a distinguishable `edc_sbx_` prefix so consumers
/// can never confuse a sandbox credential with a production one.
pub fn mint_key(grant_id: &str, label: &str, sandbox: bool) -> (ApiKeyRecord, String) {
    use rand::RngCore;
    let mut secret = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut secret);
    let prefix = if sandbox { "edc_sbx_" } else { "edc_" };
    let token = format!("{prefix}{}", hex::encode(secret));
    let digest = hex::encode(blake3::hash(token.as_bytes()).as_bytes());
    let uid = uuid::Uuid::new_v4().simple().to_string();
    (
        ApiKeyRecord {
            id: format!("key_{}", &uid[..12]),
            grant_id: grant_id.to_string(),
            token_digest: digest,
            created_at: now_ts(),
            revoked: false,
            label: label.to_string(),
            sandbox,
        },
        token,
    )
}

/// Digest a presented bearer token for lookup.
pub fn token_digest(token: &str) -> String {
    hex::encode(blake3::hash(token.as_bytes()).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(class: StakeholderClass, delay: i64) -> AccessGrant {
        AccessGrant::new(
            "prj_x",
            Grantee {
                kind: "wallet".into(),
                value: "0xabc".into(),
            },
            class,
            GrantScope::default(),
            0,
            0,
            GrantPrice::default(),
            vec!["rest".into(), "stream".into()],
            "0xowner",
            delay,
        )
    }

    #[test]
    fn investor_grant_active_immediately() {
        let g = grant(StakeholderClass::Investor, 3600);
        assert_eq!(g.status, GrantStatus::Active);
        assert!(g.is_usable(now_ts()));
    }

    #[test]
    fn regulator_grant_timelocked() {
        let g = grant(StakeholderClass::Regulator, 3600);
        assert_eq!(g.status, GrantStatus::PendingTimelock);
        let now = now_ts();
        assert!(!g.is_usable(now));
        assert!(g.is_usable(now + 3601));
    }

    #[test]
    fn expiry_enforced_at_request_time() {
        let mut g = grant(StakeholderClass::Investor, 0);
        g.expires_at = now_ts() - 1;
        assert!(!g.is_usable(now_ts()));
    }

    #[test]
    fn revocation_wins() {
        let mut g = grant(StakeholderClass::Investor, 0);
        g.status = GrantStatus::Revoked;
        assert!(!g.is_usable(now_ts()));
    }

    #[test]
    fn scope_filters() {
        let scope = GrantScope {
            facets: vec!["readings".into()],
            asset_ids: vec!["a1".into()],
            categories: vec![],
        };
        assert!(scope.allows_facet("readings"));
        assert!(!scope.allows_facet("approvals"));
        assert!(scope.allows_asset("a1", "Cities"));
        assert!(!scope.allows_asset("a2", "Cities"));
    }

    #[test]
    fn key_mint_and_verify() {
        let (rec, token) = mint_key("gr_1", "regulator key", false);
        assert!(token.starts_with("edc_"));
        assert!(!rec.sandbox);
        assert_eq!(rec.token_digest, token_digest(&token));
        assert_ne!(rec.token_digest, token_digest("edc_wrong"));
    }

    #[test]
    fn sandbox_key_distinguishable() {
        let (rec, token) = mint_key("gr_1", "integration test key", true);
        assert!(token.starts_with("edc_sbx_"));
        assert!(rec.sandbox);
    }
}
