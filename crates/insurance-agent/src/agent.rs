//! Glue: feeds → risk model → attestation → anchor.

use crate::anchor::{Anchor, AnchorError, AnchorReceipt};
use crate::attestation::{AttestationDigest, ParametricInsuranceAttestation};
use crate::config::InsuranceAgentConfig;
use crate::feeds::AssetFeed;
use crate::risk::RiskModel;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Human-friendly metrics surface for `/health`-style endpoints, tests, and
/// CLI output. Counters are monotonically increasing for the lifetime of
/// the process.
#[derive(Debug, Clone, Default)]
pub struct AgentMetrics {
    pub attestations_issued: u64,
    pub attestations_failed: u64,
    pub assets_seen: u64,
    pub assets_skipped_recent: u64,
    pub last_run_at: Option<i64>,
    pub last_run_duration_ms: Option<u128>,
    pub feeds_active: usize,
}

/// One slot in the de-dup cache: when did we last successfully attest this
/// asset, and what was the digest of that attestation.
#[derive(Debug, Clone)]
struct AttestationCacheEntry {
    last_at_unix: i64,
    last_digest: AttestationDigest,
    last_knot_string_id: String,
}

/// The agent.
pub struct InsuranceAgent {
    cfg: InsuranceAgentConfig,
    feeds: Vec<Arc<dyn AssetFeed>>,
    risk_model: RiskModel,
    anchor: Arc<dyn Anchor>,
    cache: RwLock<HashMap<String, AttestationCacheEntry>>,
    metrics: RwLock<AgentMetrics>,
}

impl InsuranceAgent {
    pub fn new(
        cfg: InsuranceAgentConfig,
        feeds: Vec<Arc<dyn AssetFeed>>,
        risk_model: RiskModel,
        anchor: Arc<dyn Anchor>,
    ) -> Self {
        let metrics = AgentMetrics {
            feeds_active: feeds.len(),
            ..Default::default()
        };
        Self {
            cfg,
            feeds,
            risk_model,
            anchor,
            cache: RwLock::new(HashMap::new()),
            metrics: RwLock::new(metrics),
        }
    }

    /// Run forever (or once if `cfg.run_once == true`).
    pub async fn run(&self) -> anyhow::Result<()> {
        info!(
            target: "insurance_agent",
            feeds = self.feeds.len(),
            interval_secs = self.cfg.interval.as_secs(),
            reattest_after_secs = self.cfg.reattest_after.as_secs(),
            agent_wallet = %self.cfg.agent_wallet,
            "starting InsuranceAgent"
        );

        loop {
            let started = std::time::Instant::now();
            let now = chrono::Utc::now().timestamp();

            match self.run_once_inner(now).await {
                Ok(summary) => info!(
                    target: "insurance_agent",
                    issued = summary.issued,
                    skipped = summary.skipped_recent,
                    failed = summary.failed,
                    seen = summary.seen,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "pass complete"
                ),
                Err(e) => error!(target: "insurance_agent", error = %e, "pass failed"),
            }

            {
                let mut m = self.metrics.write().await;
                m.last_run_at = Some(now);
                m.last_run_duration_ms = Some(started.elapsed().as_millis());
            }

            if self.cfg.run_once {
                return Ok(());
            }

            tokio::time::sleep(self.cfg.interval).await;
        }
    }

    /// Single attestation pass. Returns counters for this pass.
    pub async fn run_once(&self) -> anyhow::Result<PassSummary> {
        let now = chrono::Utc::now().timestamp();
        self.run_once_inner(now).await
    }

    async fn run_once_inner(&self, now: i64) -> anyhow::Result<PassSummary> {
        let mut summary = PassSummary::default();

        // 1. Pull every feed.
        let mut all_assets = Vec::new();
        for feed in &self.feeds {
            let name = feed.name();
            match feed.fetch().await {
                Ok(assets) => {
                    debug!(
                        target: "insurance_agent",
                        feed = name,
                        count = assets.len(),
                        "feed returned"
                    );
                    all_assets.extend(assets);
                }
                Err(e) => warn!(
                    target: "insurance_agent",
                    feed = name,
                    error = %e,
                    "feed failed; continuing with other feeds"
                ),
            }
        }
        summary.seen = all_assets.len() as u64;

        // 2. Pre-compute the reattestation cutoff (a Unix timestamp before
        //    which we should re-attest).
        let cutoff = now.saturating_sub(self.cfg.reattest_after.as_secs() as i64);

        // 3. Process each asset.
        let valid_until = now + self.cfg.attestation_validity.as_secs() as i64;
        for asset in all_assets {
            // Cache hit → skip only if *strictly* newer than the cutoff.
            // Equality means "exactly at the threshold, time to re-attest"
            // and also makes `reattest_after = 0` mean "always re-attest".
            {
                let cache = self.cache.read().await;
                if let Some(entry) = cache.get(&asset.asset_id) {
                    if entry.last_at_unix > cutoff {
                        debug!(
                            target: "insurance_agent",
                            asset_id = %asset.asset_id,
                            last_at = entry.last_at_unix,
                            cutoff,
                            "skipping — recent attestation cached"
                        );
                        summary.skipped_recent += 1;
                        continue;
                    }
                }
            }

            let profile = self.risk_model.evaluate(&asset);
            let attestation = match ParametricInsuranceAttestation::build(
                &asset,
                &profile,
                &self.cfg.agent_id,
                now,
                valid_until,
            ) {
                Ok(a) => a,
                Err(e) => {
                    warn!(
                        target: "insurance_agent",
                        asset_id = %asset.asset_id,
                        error = %e,
                        "skipping asset — attestation build failed"
                    );
                    summary.failed += 1;
                    continue;
                }
            };

            let digest = attestation.digest();
            match self.anchor.anchor(&attestation).await {
                Ok(receipt) => {
                    info!(
                        target: "insurance_agent",
                        asset_id = %asset.asset_id,
                        asset_type = %asset.asset_type,
                        premium_usd = attestation.premium_usd,
                        coverage_usd = attestation.coverage_usd,
                        knot_string_id = %receipt.knot_string_id,
                        digest = %digest.to_hex(),
                        "anchored ParametricInsuranceAttestation"
                    );
                    self.cache.write().await.insert(
                        asset.asset_id.clone(),
                        AttestationCacheEntry {
                            last_at_unix: now,
                            last_digest: digest,
                            last_knot_string_id: receipt.knot_string_id.clone(),
                        },
                    );
                    summary.issued += 1;
                }
                Err(e) => {
                    self.handle_anchor_error(&asset.asset_id, &e);
                    summary.failed += 1;
                }
            }
        }

        // 4. Push counters into the metrics surface.
        {
            let mut m = self.metrics.write().await;
            m.attestations_issued += summary.issued;
            m.attestations_failed += summary.failed;
            m.assets_seen += summary.seen;
            m.assets_skipped_recent += summary.skipped_recent;
        }

        Ok(summary)
    }

    fn handle_anchor_error(&self, asset_id: &str, err: &AnchorError) {
        match err {
            AnchorError::Rpc { code: 2002, .. } => warn!(
                target: "insurance_agent",
                asset_id,
                error = %err,
                hint = "agent ledger does not exist on the node — call rope_createLedger first",
                "anchor failed",
            ),
            AnchorError::Rpc { code: 2003, .. } => warn!(
                target: "insurance_agent",
                asset_id,
                error = %err,
                hint = "agent ledger has been deleted — re-create or rotate the wallet",
                "anchor failed",
            ),
            _ => warn!(
                target: "insurance_agent",
                asset_id,
                error = %err,
                "anchor failed"
            ),
        }
    }

    pub async fn metrics(&self) -> AgentMetrics {
        self.metrics.read().await.clone()
    }

    /// Fetch the full receipt-cache entry for a given asset id (test/debug).
    pub async fn last_anchor(&self, asset_id: &str) -> Option<AnchorReceipt> {
        let cache = self.cache.read().await;
        cache.get(asset_id).map(|e| AnchorReceipt {
            knot_string_id: e.last_knot_string_id.clone(),
            piece_count: 0,
            attestation_digest: e.last_digest,
        })
    }

    /// Build a ready-to-run agent from a config plus a shared HTTP-based
    /// `JsonRpcAnchor`. Used by the CLI.
    pub fn from_config(
        cfg: InsuranceAgentConfig,
        feeds: Vec<Arc<dyn AssetFeed>>,
    ) -> anyhow::Result<Self> {
        let anchor = Arc::new(crate::anchor::JsonRpcAnchor::new(
            cfg.rpc_url.clone(),
            cfg.agent_wallet.clone(),
            cfg.http_timeout,
        )?);
        Ok(Self::new(cfg, feeds, RiskModel::default(), anchor))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PassSummary {
    pub issued: u64,
    pub skipped_recent: u64,
    pub failed: u64,
    pub seen: u64,
}

// Silence `dead_code` on the `last_digest` field in the cache entry: it is
// intentionally kept around for future digest-based de-dup (compare current
// digest to last_digest before re-anchoring).
const _: fn(&AttestationCacheEntry) = |_e| {};
const _DEFAULT_TIMEOUT_HINT: Duration = Duration::from_secs(30);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchor::mock::MockAnchor;
    use crate::feeds::{tanastok, AssetSource, FeedError, TokenizedAsset};
    use async_trait::async_trait;

    /// In-memory test feed.
    struct StaticFeed {
        name: &'static str,
        assets: Vec<TokenizedAsset>,
    }

    #[async_trait]
    impl AssetFeed for StaticFeed {
        fn name(&self) -> &str {
            self.name
        }
        async fn fetch(&self) -> Result<Vec<TokenizedAsset>, FeedError> {
            Ok(self.assets.clone())
        }
    }

    fn assets_from_handover() -> Vec<TokenizedAsset> {
        // Three asset classes — GOLD_MINE, FORESTRY, REAL_ESTATE — wide
        // enough to exercise the per-type rate-card.
        vec![
            TokenizedAsset {
                asset_id: "kibali".into(),
                name: "Kibali Gold Mine".into(),
                asset_type: "GOLD_MINE".into(),
                location: Some("Democratic Republic of Congo".into()),
                valuation_usd: 10_000_000.0,
                is_verified: true,
                chain_id: Some(271828),
                dcnft_addr: Some("0xdcnft1".into()),
                erc3643_addr: Some("0xerc3643_1".into()),
                source: AssetSource::Tanastok,
            },
            TokenizedAsset {
                asset_id: "amazon-7".into(),
                name: "Amazon Forest Plot 7".into(),
                asset_type: "FORESTRY".into(),
                location: Some("Brazil".into()),
                valuation_usd: 25_000_000.0,
                is_verified: true,
                chain_id: Some(271828),
                dcnft_addr: Some("0xdcnft2".into()),
                erc3643_addr: Some("0xerc3643_2".into()),
                source: AssetSource::Tanastok,
            },
            TokenizedAsset {
                asset_id: "paris-flat-12".into(),
                name: "Paris Flat 12".into(),
                asset_type: "REAL_ESTATE".into(),
                location: Some("France".into()),
                valuation_usd: 1_500_000.0,
                is_verified: true,
                chain_id: Some(271828),
                dcnft_addr: Some("0xdcnft3".into()),
                erc3643_addr: Some("0xerc3643_3".into()),
                source: AssetSource::Tanastok,
            },
        ]
    }

    #[tokio::test]
    async fn issues_one_attestation_per_asset() {
        let feeds: Vec<Arc<dyn AssetFeed>> = vec![Arc::new(StaticFeed {
            name: "test",
            assets: assets_from_handover(),
        })];
        let mock = Arc::new(MockAnchor::new());
        let cfg = InsuranceAgentConfig {
            run_once: true,
            ..Default::default()
        };
        let agent = InsuranceAgent::new(cfg, feeds, RiskModel::default(), mock.clone());

        let summary = agent.run_once().await.unwrap();
        assert_eq!(summary.seen, 3);
        assert_eq!(summary.issued, 3);
        assert_eq!(summary.skipped_recent, 0);
        assert_eq!(summary.failed, 0);
        assert_eq!(mock.count(), 3);

        // Each anchored attestation matches the source asset.
        let anchored = mock.anchored.lock().unwrap().clone();
        let kibali = anchored
            .iter()
            .find(|a| a.asset_id == "kibali")
            .expect("kibali anchored");
        assert_eq!(kibali.asset_type, "GOLD_MINE");
        assert!(kibali.premium_bps > 0);
        let forestry = anchored.iter().find(|a| a.asset_id == "amazon-7").unwrap();
        assert_eq!(forestry.asset_type, "FORESTRY");
        let real_estate = anchored
            .iter()
            .find(|a| a.asset_id == "paris-flat-12")
            .unwrap();
        assert_eq!(real_estate.asset_type, "REAL_ESTATE");
    }

    #[tokio::test]
    async fn second_pass_within_window_is_skipped() {
        let feeds: Vec<Arc<dyn AssetFeed>> = vec![Arc::new(StaticFeed {
            name: "test",
            assets: assets_from_handover(),
        })];
        let mock = Arc::new(MockAnchor::new());
        let cfg = InsuranceAgentConfig {
            run_once: true,
            reattest_after: Duration::from_secs(86_400),
            ..Default::default()
        };
        let agent = InsuranceAgent::new(cfg, feeds, RiskModel::default(), mock.clone());

        let s1 = agent.run_once().await.unwrap();
        assert_eq!(s1.issued, 3);

        let s2 = agent.run_once().await.unwrap();
        assert_eq!(s2.issued, 0);
        assert_eq!(s2.skipped_recent, 3);
        // Still only 3 anchored — we did not hit the network the second time.
        assert_eq!(mock.count(), 3);
    }

    #[tokio::test]
    async fn anchor_failures_count_as_failed_not_issued() {
        let feeds: Vec<Arc<dyn AssetFeed>> = vec![Arc::new(StaticFeed {
            name: "test",
            assets: assets_from_handover(),
        })];
        let mock = Arc::new(MockAnchor::fail_after(2));
        let agent = InsuranceAgent::new(
            InsuranceAgentConfig {
                run_once: true,
                ..Default::default()
            },
            feeds,
            RiskModel::default(),
            mock.clone(),
        );

        let summary = agent.run_once().await.unwrap();
        assert_eq!(summary.seen, 3);
        assert_eq!(summary.issued, 2);
        assert_eq!(summary.failed, 1);
    }

    #[tokio::test]
    async fn metrics_accumulate_over_passes() {
        let feeds: Vec<Arc<dyn AssetFeed>> = vec![Arc::new(StaticFeed {
            name: "test",
            assets: assets_from_handover(),
        })];
        let mock = Arc::new(MockAnchor::new());
        let cfg = InsuranceAgentConfig {
            run_once: true,
            reattest_after: Duration::from_secs(0), // re-attest every pass
            ..Default::default()
        };
        let agent = InsuranceAgent::new(cfg, feeds, RiskModel::default(), mock.clone());

        agent.run_once().await.unwrap();
        agent.run_once().await.unwrap();

        let m = agent.metrics().await;
        assert_eq!(m.attestations_issued, 6);
        assert_eq!(m.assets_seen, 6);
        assert_eq!(m.feeds_active, 1);
    }

    #[tokio::test]
    async fn integrates_with_tanastok_parser() {
        // Drive the agent off the Tanastok-shaped JSON via a hand-rolled
        // feed so the parse-then-attest path is exercised end-to-end.
        struct CannedTanastok {
            body: &'static str,
        }
        #[async_trait]
        impl AssetFeed for CannedTanastok {
            fn name(&self) -> &str {
                "tanastok-canned"
            }
            async fn fetch(&self) -> Result<Vec<TokenizedAsset>, FeedError> {
                tanastok::parse_response(self.body)
            }
        }

        let body = r#"{
            "success": true,
            "data": [
                {
                    "id": "x", "assetType": "REAL_ESTATE",
                    "value": 100000, "isVerified": true,
                    "location": "United Kingdom", "chainId": 271828,
                    "dcnft": {"contractAddress": "0xa"},
                    "erc3643": {"contractAddress": "0xb"}
                }
            ]
        }"#;

        let feeds: Vec<Arc<dyn AssetFeed>> = vec![Arc::new(CannedTanastok { body })];
        let mock = Arc::new(MockAnchor::new());
        let cfg = InsuranceAgentConfig {
            run_once: true,
            ..Default::default()
        };
        let agent = InsuranceAgent::new(cfg, feeds, RiskModel::default(), mock.clone());

        let s = agent.run_once().await.unwrap();
        assert_eq!(s.issued, 1);
        let att = mock.anchored.lock().unwrap()[0].clone();
        assert_eq!(att.asset_id, "x");
        assert_eq!(att.asset_type, "REAL_ESTATE");
        // 90 bps * 0.85 (UK) = 76.5 → 77 bps
        assert_eq!(att.premium_bps, 77);
    }
}
