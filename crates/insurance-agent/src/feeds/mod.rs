//! Pluggable asset feeds.
//!
//! Every feed produces a list of [`TokenizedAsset`] values that the agent
//! evaluates against the parametric risk model.
//!
//! Two feeds ship with this crate:
//!
//! - [`tanastok::TanastokFeed`] — live HTTP feed of Tanastok DCNFT/ERC-3643
//!   pairs.
//! - [`naturaproof::NaturaProofStubFeed`] — typed stub that returns an empty
//!   list. The real biodiversity-proof feed lives in the NaturaProof project.
//!
//! Add a new feed by implementing [`AssetFeed`].

pub mod naturaproof;
pub mod tanastok;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Common identifier for an asset across feeds.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssetSource {
    Tanastok,
    NaturaProof,
    Other(String),
}

impl AssetSource {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Tanastok => "tanastok",
            Self::NaturaProof => "naturaproof",
            Self::Other(s) => s.as_str(),
        }
    }
}

/// Normalized representation of a tokenized real-world asset.
///
/// Built from a Tanastok or NaturaProof API response. The risk model and
/// attestation builder only consume this type — they never see raw API
/// shapes. This keeps the wire format orthogonal to the underwriting
/// pipeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizedAsset {
    /// Stable asset identifier (Tanastok `id` or the source-specific ID).
    pub asset_id: String,

    /// Human-readable name.
    pub name: String,

    /// Asset class: `GOLD_MINE`, `FORESTRY`, `REAL_ESTATE`, …
    pub asset_type: String,

    /// ISO country / sub-region in which the asset is located. Used by the
    /// jurisdiction multiplier in the risk model.
    pub location: Option<String>,

    /// Total appraised valuation in USD.
    pub valuation_usd: f64,

    /// Whether the asset's verification status is set on-chain.
    pub is_verified: bool,

    /// Datachain Rope `chainId` (271828 for mainnet).
    pub chain_id: Option<u64>,

    /// DCNFT (ERC-721) contract address — title deed.
    pub dcnft_addr: Option<String>,

    /// ERC-3643 contract address — security token (fractional shares).
    pub erc3643_addr: Option<String>,

    /// Where this record came from.
    pub source: AssetSource,
}

/// Errors any feed implementation can return.
#[derive(Debug, Error)]
pub enum FeedError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("invalid JSON in feed response: {0}")]
    InvalidJson(String),

    #[error("feed reported failure: {0}")]
    Upstream(String),

    #[error("feed not configured: {0}")]
    NotConfigured(String),
}

/// Plug-in contract for any feed that can produce [`TokenizedAsset`] records.
#[async_trait]
pub trait AssetFeed: Send + Sync {
    /// Stable identifier for the feed (used in logs and metrics).
    fn name(&self) -> &str;

    /// Fetch the current asset list. Implementations should treat transient
    /// errors as their own concern (e.g. retries) and surface only terminal
    /// failures.
    async fn fetch(&self) -> Result<Vec<TokenizedAsset>, FeedError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_source_strings() {
        assert_eq!(AssetSource::Tanastok.as_str(), "tanastok");
        assert_eq!(AssetSource::NaturaProof.as_str(), "naturaproof");
        assert_eq!(AssetSource::Other("foo".into()).as_str(), "foo");
    }
}
