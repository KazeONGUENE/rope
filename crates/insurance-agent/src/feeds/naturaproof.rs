//! NaturaProof biodiversity-proof feed — typed stub.
//!
//! TODO(naturaproof): replace this with a real client once the NaturaProof
//! verification API stabilises. The integration lives in a separate project
//! (https://naturaproof.io). This crate only needs the [`AssetFeed`] shape to
//! be filled in — the agent loop already calls every registered feed in turn.
//!
//! Until that happens we ship a typed stub that returns `Vec::new()`. That
//! way `cargo run -p insurance-agent serve` produces a clean, observable
//! "0 NaturaProof assets" line in the logs rather than failing or pretending
//! to have data.

use crate::feeds::{AssetFeed, FeedError, TokenizedAsset};
use async_trait::async_trait;

/// Stub NaturaProof feed.
///
/// Always returns an empty list. Construct it with [`Self::new`] and feed it
/// into [`crate::InsuranceAgent::with_feed`] to keep the wiring in place
/// until a real client lands.
pub struct NaturaProofStubFeed {
    /// Optional endpoint string. Stored for telemetry only — the stub never
    /// hits the network.
    pub endpoint: Option<String>,
}

impl NaturaProofStubFeed {
    pub fn new() -> Self {
        Self { endpoint: None }
    }

    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
        }
    }
}

impl Default for NaturaProofStubFeed {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AssetFeed for NaturaProofStubFeed {
    fn name(&self) -> &str {
        "naturaproof-stub"
    }

    async fn fetch(&self) -> Result<Vec<TokenizedAsset>, FeedError> {
        tracing::debug!(
            target: "insurance_agent::feed::naturaproof",
            endpoint = ?self.endpoint,
            "naturaproof stub feed returning empty list (TODO: real impl)"
        );
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stub_returns_empty_list() {
        let feed = NaturaProofStubFeed::new();
        let assets = feed.fetch().await.expect("stub never errors");
        assert!(assets.is_empty());
        assert_eq!(feed.name(), "naturaproof-stub");
    }

    #[tokio::test]
    async fn stub_records_endpoint_for_telemetry() {
        let feed = NaturaProofStubFeed::with_endpoint("https://api.naturaproof.io/v1/proofs");
        assert_eq!(
            feed.endpoint.as_deref(),
            Some("https://api.naturaproof.io/v1/proofs")
        );
        assert!(feed.fetch().await.unwrap().is_empty());
    }
}
