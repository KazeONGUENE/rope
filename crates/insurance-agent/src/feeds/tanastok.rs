//! Tanastok tokenized-assets feed.
//!
//! Fetches the public API at
//! `https://tanastok.io/api/v1/tokenized-assets?limit=500` (or any URL the
//! operator passes) and turns each entry into a [`TokenizedAsset`].
//!
//! The wire shape comes straight from the workspace handover rule
//! `handover-tanastok-tokenized-assets-for-dcscan-2026-03-30.mdc`. Keep the
//! pure-function parser [`parse_response`] easy to test against the example
//! JSON quoted in that handover.

use crate::feeds::{AssetFeed, AssetSource, FeedError, TokenizedAsset};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;

/// HTTP-backed Tanastok feed.
pub struct TanastokFeed {
    url: String,
    client: Client,
}

impl TanastokFeed {
    /// Build a feed pointed at the given URL with the given HTTP timeout.
    pub fn new(url: impl Into<String>, timeout: Duration) -> Result<Self, FeedError> {
        let client = Client::builder()
            .timeout(timeout)
            .user_agent(format!(
                "insurance-agent/{} (+https://datachain.network)",
                crate::VERSION
            ))
            .build()?;
        Ok(Self {
            url: url.into(),
            client,
        })
    }

    /// Build a feed using a pre-configured HTTP client. Lets callers control
    /// proxies, mTLS, custom headers, etc.
    pub fn with_client(url: impl Into<String>, client: Client) -> Self {
        Self {
            url: url.into(),
            client,
        }
    }
}

#[async_trait]
impl AssetFeed for TanastokFeed {
    fn name(&self) -> &str {
        "tanastok"
    }

    async fn fetch(&self) -> Result<Vec<TokenizedAsset>, FeedError> {
        tracing::debug!(target: "insurance_agent::feed::tanastok", url = %self.url, "fetching tokenized assets");
        let body = self.client.get(&self.url).send().await?.text().await?;
        parse_response(&body)
    }
}

/// Parses a Tanastok `/api/v1/tokenized-assets` response body into the
/// crate's normalized [`TokenizedAsset`] shape.
///
/// Pure function on a `&str` so unit tests can hand it a fixture without
/// touching the network. Tolerates partial fields — anything missing falls
/// back to a sensible default and the asset is still emitted.
pub fn parse_response(body: &str) -> Result<Vec<TokenizedAsset>, FeedError> {
    let envelope: Envelope = serde_json::from_str(body)
        .map_err(|e| FeedError::InvalidJson(format!("decoding envelope: {e}")))?;

    if !envelope.success {
        return Err(FeedError::Upstream(
            "Tanastok API returned success=false".to_string(),
        ));
    }

    Ok(envelope
        .data
        .into_iter()
        .map(TokenizedAsset::from)
        .collect())
}

#[derive(Debug, Deserialize)]
struct Envelope {
    success: bool,
    #[serde(default)]
    data: Vec<RawAsset>,
}

#[derive(Debug, Deserialize)]
struct RawAsset {
    id: String,
    #[serde(default)]
    name: String,
    #[serde(rename = "assetType", default)]
    asset_type: String,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    value: Option<f64>,
    #[serde(rename = "isVerified", default)]
    is_verified: bool,
    #[serde(rename = "chainId", default)]
    chain_id: Option<u64>,
    #[serde(default)]
    dcnft: Option<RawDcnft>,
    #[serde(default)]
    erc3643: Option<RawErc3643>,
}

#[derive(Debug, Deserialize)]
struct RawDcnft {
    #[serde(rename = "contractAddress", default)]
    contract_address: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RawErc3643 {
    #[serde(rename = "contractAddress", default)]
    contract_address: Option<String>,
}

impl From<RawAsset> for TokenizedAsset {
    fn from(raw: RawAsset) -> Self {
        TokenizedAsset {
            asset_id: raw.id,
            name: raw.name,
            asset_type: raw.asset_type,
            location: raw.location,
            valuation_usd: raw.value.unwrap_or(0.0),
            is_verified: raw.is_verified,
            chain_id: raw.chain_id,
            dcnft_addr: raw.dcnft.and_then(|d| d.contract_address),
            erc3643_addr: raw.erc3643.and_then(|e| e.contract_address),
            source: AssetSource::Tanastok,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture lifted (and lightly trimmed) from the Tanastok handover rule.
    /// Two assets, two asset types, full DCNFT + ERC-3643 pair on each.
    const HANDOVER_FIXTURE: &str = r#"{
      "success": true,
      "count": 2,
      "data": [
        {
          "id": "featured-kibali-gold-mine",
          "name": "Kibali Gold Mine, Congo DRC",
          "assetType": "GOLD_MINE",
          "value": 10000000053,
          "totalShares": 19736,
          "location": "Democratic Republic of Congo",
          "isVerified": true,
          "chainId": 271828,
          "network": "Datachain Rope",
          "dcnft": {
            "contractAddress": "0x91f884D436858ad221436573BC2cB5117E27e564",
            "tokenId": "1",
            "standard": "ERC-721"
          },
          "erc3643": {
            "contractAddress": "0x2D16be771cB30AEedD9913b70b6237a832828bbB",
            "tokenSymbol": "KIBALI",
            "standard": "ERC-3643"
          }
        },
        {
          "id": "featured-amazon-forest-plot-7",
          "name": "Amazon Forest Plot 7, Brazil",
          "assetType": "FORESTRY",
          "value": 25000000,
          "location": "Brazil",
          "isVerified": true,
          "chainId": 271828,
          "dcnft": { "contractAddress": "0xAAAA000000000000000000000000000000000007" },
          "erc3643": { "contractAddress": "0xBBBB000000000000000000000000000000000007" }
        }
      ]
    }"#;

    #[test]
    fn parses_handover_fixture() {
        let assets = parse_response(HANDOVER_FIXTURE).expect("parse fixture");
        assert_eq!(assets.len(), 2);

        let kibali = &assets[0];
        assert_eq!(kibali.asset_id, "featured-kibali-gold-mine");
        assert_eq!(kibali.asset_type, "GOLD_MINE");
        assert!(kibali.is_verified);
        assert_eq!(kibali.chain_id, Some(271828));
        assert_eq!(kibali.valuation_usd, 10_000_000_053.0);
        assert_eq!(
            kibali.dcnft_addr.as_deref(),
            Some("0x91f884D436858ad221436573BC2cB5117E27e564")
        );
        assert_eq!(
            kibali.erc3643_addr.as_deref(),
            Some("0x2D16be771cB30AEedD9913b70b6237a832828bbB")
        );
        assert_eq!(kibali.source, AssetSource::Tanastok);

        let forest = &assets[1];
        assert_eq!(forest.asset_type, "FORESTRY");
        assert_eq!(forest.valuation_usd, 25_000_000.0);
    }

    #[test]
    fn rejects_failed_envelope() {
        let body = r#"{"success": false, "data": []}"#;
        match parse_response(body) {
            Err(FeedError::Upstream(_)) => {}
            other => panic!("expected Upstream error, got {other:?}"),
        }
    }

    #[test]
    fn rejects_garbage() {
        let body = "not json";
        match parse_response(body) {
            Err(FeedError::InvalidJson(_)) => {}
            other => panic!("expected InvalidJson, got {other:?}"),
        }
    }

    #[test]
    fn tolerates_missing_optional_fields() {
        let body = r#"{
          "success": true,
          "data": [
            {"id": "minimal-1", "assetType": "REAL_ESTATE"}
          ]
        }"#;
        let assets = parse_response(body).unwrap();
        assert_eq!(assets.len(), 1);
        let a = &assets[0];
        assert_eq!(a.asset_id, "minimal-1");
        assert_eq!(a.asset_type, "REAL_ESTATE");
        assert!(!a.is_verified);
        assert_eq!(a.valuation_usd, 0.0);
        assert!(a.dcnft_addr.is_none());
        assert!(a.erc3643_addr.is_none());
    }
}
