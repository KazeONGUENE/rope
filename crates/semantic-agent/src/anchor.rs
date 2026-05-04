//! Submits signed [`crate::IndexCheckpointTestimony`] knots to the
//! rope-node via `rope_appendToLedger`.
//!
//! The agent's wallet must already have a personal ledger; the rope-node
//! will return RPC error `2002 (No ledger found for this address)` if
//! it doesn't. The bootstrap flow is:
//!
//! ```bash
//! cast rpc rope_createPersonalLedger 0x000000000000000000000000000000000000C001 \
//!   --rpc-url http://127.0.0.1:8545
//! ```
//!
//! Run that once per fresh chain reset, then start the agent.
//!
//! Signing: today this submits the testimony as plain
//! `metadata` fields. Phase 2 will require a signed payload (canon
//! v1.1 §6 — same auth model as `rope_untieKnot`); when that lands,
//! [`AnchorSubmitter::sign_payload`] is the hook to wire in.

use crate::checkpoint::{CheckpointBuilder, IndexCheckpointTestimony};
use crate::config::AgentConfig;
use crate::rpc::RpcClient;
use crate::search::SearchService;
use crate::AgentMetrics;
use parking_lot::RwLock;
use serde::Serialize;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Plain marker type so callers can distinguish the "submitted" outcome
/// from a stub one in tests.
#[derive(Clone, Debug, Serialize)]
pub struct AnchorOutcome {
    pub testimony: IndexCheckpointTestimony,
    pub knot_id: String,
    pub anchored_at: i64,
}

/// Pluggable interface so tests can substitute a fake RPC sink.
#[async_trait::async_trait]
pub trait Anchor: Send + Sync {
    async fn anchor(
        &self,
        owner: &str,
        testimony: IndexCheckpointTestimony,
    ) -> anyhow::Result<AnchorOutcome>;
}

/// Production [`Anchor`] backed by a [`RpcClient`].
pub struct AnchorSubmitter {
    config: Arc<AgentConfig>,
    rpc: Arc<RpcClient>,
    search: Arc<SearchService>,
    metrics: Arc<RwLock<AgentMetrics>>,
}

impl AnchorSubmitter {
    pub fn new(
        config: Arc<AgentConfig>,
        rpc: Arc<RpcClient>,
        search: Arc<SearchService>,
        metrics: Arc<RwLock<AgentMetrics>>,
    ) -> Self {
        Self {
            config,
            rpc,
            search,
            metrics,
        }
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Hook for signing the testimony's canonical bytes. Today returns
    /// `None` (Phase 1, trusted-proxy auth). When Phase 2 lands and the
    /// rope-node enforces signed appends, this is where ed25519 +
    /// Dilithium signing slots in. Keeping the hook in the trait
    /// surface means we can land the on-chain check first and add the
    /// signing logic without changing call sites.
    pub fn sign_payload(&self, _testimony: &IndexCheckpointTestimony) -> Option<Vec<u8>> {
        None
    }

    /// Build a fresh checkpoint and submit it. Returns the
    /// [`AnchorOutcome`] containing the knot id assigned by the node.
    /// Honors `config.read_only` — when set, returns Ok without
    /// touching the network.
    pub async fn build_and_anchor(
        &self,
        last_string_id: Option<String>,
    ) -> anyhow::Result<Option<AnchorOutcome>> {
        if self.config.read_only {
            debug!("read-only mode — skipping checkpoint anchor");
            return Ok(None);
        }
        let builder = CheckpointBuilder::new(self.config.clone(), self.search.clone());
        let (testimony, _root) = builder.build(last_string_id)?;
        let outcome = self.anchor(&self.config.identity.wallet, testimony).await?;
        let mut m = self.metrics.write();
        m.checkpoint_count = m.checkpoint_count.saturating_add(1);
        m.last_checkpoint_at = Some(outcome.anchored_at);
        m.last_checkpoint_root = Some(outcome.testimony.merkle_root.clone());
        m.last_checkpoint_total_indexed = outcome.testimony.total_indexed;
        m.last_anchor_knot_id = Some(outcome.knot_id.clone());
        m.last_anchor_at = Some(outcome.anchored_at);
        Ok(Some(outcome))
    }

    /// Spawn the cadence loop that calls [`Self::build_and_anchor`]
    /// every `config.checkpoint_interval`.
    pub fn spawn_checkpoint_loop(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let interval = self.config.checkpoint_interval;
        tokio::spawn(async move {
            // Wait one full interval before the first anchor so the
            // indexer has a chance to populate the index.
            tokio::time::sleep(interval).await;
            loop {
                let last_string_id = {
                    let m = self.metrics.read();
                    m.last_indexed_string_id.clone()
                };
                match self.build_and_anchor(last_string_id).await {
                    Ok(Some(outcome)) => info!(
                        knot = %outcome.knot_id,
                        merkle_root = %outcome.testimony.merkle_root,
                        total_indexed = outcome.testimony.total_indexed,
                        "anchored IndexCheckpointTestimony"
                    ),
                    Ok(None) => debug!("checkpoint skipped (read-only)"),
                    Err(e) => {
                        warn!(error = %e, "checkpoint anchor failed");
                        let mut m = self.metrics.write();
                        m.anchor_errors = m.anchor_errors.saturating_add(1);
                    }
                }
                tokio::time::sleep(interval).await;
            }
        })
    }
}

#[async_trait::async_trait]
impl Anchor for AnchorSubmitter {
    async fn anchor(
        &self,
        owner: &str,
        testimony: IndexCheckpointTestimony,
    ) -> anyhow::Result<AnchorOutcome> {
        let interaction = testimony.to_interaction();
        // Brief retry — production rope-nodes occasionally return
        // transient errors during the per-wallet head-lock window.
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..3 {
            match self.rpc.append_to_ledger(owner, interaction.clone()).await {
                Ok(resp) => {
                    let knot_id = resp
                        .get("hash")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if knot_id.is_empty() {
                        return Err(anyhow::anyhow!(
                            "rope_appendToLedger returned no `hash` field: {resp}"
                        ));
                    }
                    return Ok(AnchorOutcome {
                        testimony,
                        knot_id,
                        anchored_at: chrono::Utc::now().timestamp(),
                    });
                }
                Err(e) => {
                    last_err = Some(anyhow::anyhow!("attempt {}: {e}", attempt + 1));
                    tokio::time::sleep(Duration::from_millis(200 * (attempt + 1))).await;
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("anchor failed with unknown error")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::IndexCheckpointTestimony;
    use crate::AgentMetrics;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fresh_search() -> Arc<SearchService> {
        let dir = tempfile::tempdir().unwrap().keep();
        Arc::new(SearchService::open_or_create(dir).unwrap())
    }

    fn build_anchor(rpc_url: String) -> AnchorSubmitter {
        let cfg = Arc::new(AgentConfig {
            rpc_url: rpc_url.clone(),
            checkpoint_interval: Duration::from_secs(60),
            read_only: false,
            ..AgentConfig::default()
        });
        let rpc = Arc::new(RpcClient::new(rpc_url, Duration::from_secs(2)).unwrap());
        let search = fresh_search();
        let metrics = Arc::new(RwLock::new(AgentMetrics::default()));
        AnchorSubmitter::new(cfg, rpc, search, metrics)
    }

    fn dummy_testimony() -> IndexCheckpointTestimony {
        IndexCheckpointTestimony {
            event_type: "IndexCheckpointTestimony/v1".into(),
            agent_id: "semantic".into(),
            agent_wallet: crate::CANONICAL_AGENT_WALLET.into(),
            merkle_root: "0xdeadbeef".into(),
            total_indexed: 7,
            last_string_id: Some("0xowner".into()),
            schema_version: 1,
            checkpoint_at: 1_700_000_000,
        }
    }

    #[tokio::test]
    async fn anchor_succeeds_and_returns_knot_hash() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "result": {"index": 12, "hash": "0xanchored-knot-id"},
            })))
            .mount(&server)
            .await;
        let submitter = build_anchor(server.uri());
        let out = submitter
            .anchor(crate::CANONICAL_AGENT_WALLET, dummy_testimony())
            .await
            .unwrap();
        assert_eq!(out.knot_id, "0xanchored-knot-id");
        assert_eq!(out.testimony.total_indexed, 7);
    }

    #[tokio::test]
    async fn anchor_retries_then_fails() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32603, "message": "transient"},
            })))
            .mount(&server)
            .await;
        let submitter = build_anchor(server.uri());
        let err = submitter
            .anchor(crate::CANONICAL_AGENT_WALLET, dummy_testimony())
            .await
            .unwrap_err();
        let s = format!("{err}");
        assert!(s.contains("attempt 3"), "expected three attempts, got: {s}");
    }

    #[tokio::test]
    async fn read_only_skips_anchor() {
        let server = MockServer::start().await;
        let cfg = Arc::new(AgentConfig {
            rpc_url: server.uri(),
            read_only: true,
            ..AgentConfig::default()
        });
        let rpc = Arc::new(RpcClient::new(server.uri(), Duration::from_secs(2)).unwrap());
        let search = fresh_search();
        let metrics = Arc::new(RwLock::new(AgentMetrics::default()));
        let submitter = AnchorSubmitter::new(cfg, rpc, search, metrics);
        let out = submitter.build_and_anchor(None).await.unwrap();
        assert!(out.is_none());
    }
}
