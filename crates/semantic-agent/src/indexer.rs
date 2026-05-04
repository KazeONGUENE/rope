//! Pulls knots from the rope-node JSON-RPC surface and writes them to
//! the local tantivy index.
//!
//! ## Algorithm (per poll)
//!
//! 1. `rope_globalStats` → snapshot total knots / strings (used for
//!    progress reporting and the metrics endpoint).
//! 2. Walk pages of `rope_listStrings(offset=0, limit=N)` until we
//!    have seen every active string. Sorted by `last_anchored_at`
//!    desc by the node, so the freshest strings come first.
//! 3. For each string descriptor where `head_knot_id` is unseen by us
//!    or has more knots than we've indexed:
//!    - For wallet kind: `rope_getStringWithKnots(wallet)` returns the
//!      full ordered list of knots (with `active`/`tombstone` status).
//!    - For other kinds (contract / asset / did / cord), the node's
//!      RPC layer is wallet-only as of Quipu Canon v1.2; we index the
//!      genesis knot directly from the descriptor (this is a best-
//!      effort projection — see honest caveat in lib.rs).
//! 4. For every newly seen knot, build a [`KnotIndexEntry`], enrich it
//!    via [`crate::event_type::enrich`], and submit a batch to
//!    [`SearchService::index_entries`].
//!
//! The indexer is idempotent: re-indexing the same knot is an upsert.
//! That's a deliberate choice — it lets the agent recover from a
//! restart by simply re-running the loop; eventual consistency falls
//! out of the upsert semantics.

use crate::config::AgentConfig;
use crate::rpc::RpcClient;
use crate::search::SearchService;
use crate::{event_type, AgentMetrics, KnotIndexEntry};
use parking_lot::{Mutex, RwLock};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tracing::{debug, info, warn};

/// Snapshot of indexer counters — exposed via [`crate::SemanticAgent::metrics`].
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct IndexerStats {
    pub strings_seen: u64,
    pub knots_seen: u64,
    pub last_globalstats: Option<Value>,
}

/// One pass through the indexer loop.
#[derive(Debug, Default, serde::Serialize)]
pub struct PollOutcome {
    pub strings_walked: u64,
    pub knots_indexed: u64,
    pub knots_skipped: u64,
    pub last_string_id: Option<String>,
}

pub struct Indexer {
    config: Arc<AgentConfig>,
    rpc: Arc<RpcClient>,
    search: Arc<SearchService>,
    metrics: Arc<RwLock<AgentMetrics>>,
    /// In-memory dedup set — `(string_id, knot_id)` we've already
    /// upserted in this process. Persistent dedup is handled by the
    /// tantivy upsert; this set just avoids redundant tantivy work.
    seen: Mutex<HashSet<(String, String)>>,
    stats: Mutex<IndexerStats>,
}

impl Indexer {
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
            seen: Mutex::new(HashSet::new()),
            stats: Mutex::new(IndexerStats::default()),
        }
    }

    /// Stats snapshot. Read lock only — cheap for `/v1/metrics`.
    pub fn stats(&self) -> IndexerStats {
        self.stats.lock().clone()
    }

    /// Spawn the indexer's poll loop. Returns immediately; the loop
    /// runs until `tokio::task::JoinHandle::abort()` is invoked.
    pub fn spawn_poll_loop(self: Arc<Self>) -> tokio::task::JoinHandle<()> {
        let interval = self.config.poll_interval;
        tokio::spawn(async move {
            // Tiny upfront jitter so two agents started in sync don't
            // hammer the node in lockstep.
            tokio::time::sleep(Duration::from_millis(250)).await;
            loop {
                match self.poll_once().await {
                    Ok(outcome) => {
                        if outcome.knots_indexed > 0 {
                            info!(
                                indexed = outcome.knots_indexed,
                                walked = outcome.strings_walked,
                                "indexer pass complete"
                            );
                        } else {
                            debug!(
                                walked = outcome.strings_walked,
                                "indexer pass: no new knots"
                            );
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "indexer pass failed");
                        let mut m = self.metrics.write();
                        m.indexer_errors = m.indexer_errors.saturating_add(1);
                    }
                }
                tokio::time::sleep(interval).await;
            }
        })
    }

    /// One synchronous pass. Returns the [`PollOutcome`] so callers
    /// (and tests) can verify behaviour without observing logs.
    pub async fn poll_once(&self) -> anyhow::Result<PollOutcome> {
        let mut outcome = PollOutcome::default();

        // 1. Snapshot global stats (used for metrics + progress).
        if let Ok(stats) = self.rpc.global_stats().await {
            self.stats.lock().last_globalstats = Some(stats);
        }

        // 2. Walk pages of strings.
        let mut offset: u64 = 0;
        let limit = self.config.list_strings_limit;
        let mut total_seen = 0u64;
        let mut last_string_id: Option<String> = None;

        loop {
            let page = self.rpc.list_strings(None, offset, limit).await?;
            let strings = page
                .get("strings")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            let total = page.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
            if strings.is_empty() {
                break;
            }
            for desc in &strings {
                outcome.strings_walked = outcome.strings_walked.saturating_add(1);
                let kind = desc
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .unwrap_or("wallet")
                    .to_string();
                let string_id = desc
                    .get("string_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                last_string_id = Some(string_id.clone());

                let (indexed, skipped) = self
                    .index_one_string(&kind, &string_id, desc)
                    .await
                    .unwrap_or_else(|e| {
                        warn!(string_id = %string_id, error = %e, "string indexing failed");
                        (0, 0)
                    });
                outcome.knots_indexed = outcome.knots_indexed.saturating_add(indexed);
                outcome.knots_skipped = outcome.knots_skipped.saturating_add(skipped);

                if outcome.knots_indexed >= self.config.max_knots_per_poll as u64 {
                    debug!(
                        cap = self.config.max_knots_per_poll,
                        "max_knots_per_poll reached"
                    );
                    break;
                }
            }
            total_seen = total_seen.saturating_add(strings.len() as u64);
            if total_seen >= total
                || strings.len() < limit as usize
                || outcome.knots_indexed >= self.config.max_knots_per_poll as u64
            {
                break;
            }
            offset = offset.saturating_add(limit as u64);
        }

        let mut s = self.stats.lock();
        s.strings_seen = s.strings_seen.saturating_add(outcome.strings_walked);
        s.knots_seen = s.knots_seen.saturating_add(outcome.knots_indexed);
        drop(s);
        outcome.last_string_id = last_string_id;
        Ok(outcome)
    }

    /// Index every knot on `string_id`'s string. Returns
    /// `(indexed, skipped)`. `skipped` counts knots already seen in
    /// the in-memory dedup set.
    async fn index_one_string(
        &self,
        kind: &str,
        string_id: &str,
        desc: &Value,
    ) -> anyhow::Result<(u64, u64)> {
        let mut entries: Vec<KnotIndexEntry> = Vec::new();
        let mut skipped: u64 = 0;
        let now = chrono::Utc::now().timestamp();
        let last_anchored_at = desc
            .get("last_anchored_at")
            .or_else(|| desc.get("last_appended_at"))
            .and_then(|v| v.as_i64())
            .unwrap_or(now);

        if kind == "wallet" {
            // Walk the full string. The node returns active + tombstone
            // entries in genesis-first order.
            let v = self
                .rpc
                .get_string_with_knots(string_id)
                .await
                .map_err(|e| anyhow::anyhow!("getStringWithKnots({string_id}): {e}"))?;
            let knots = v
                .get("knots")
                .and_then(|v| v.as_array())
                .cloned()
                .unwrap_or_default();
            for k in &knots {
                let knot_id = k
                    .get("string_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                if knot_id.is_empty() {
                    continue;
                }
                let key = (string_id.to_string(), knot_id.clone());
                if self.seen.lock().contains(&key) {
                    skipped = skipped.saturating_add(1);
                    continue;
                }
                let knot_index = k.get("knot_index").and_then(|v| v.as_u64()).unwrap_or(0);
                let status = k
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("active")
                    .to_string();
                let knot_ts = k
                    .get("tombstone")
                    .and_then(|t| t.get("untied_at"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(last_anchored_at);

                let mut entry = KnotIndexEntry {
                    knot_id,
                    string_id: string_id.to_string(),
                    string_kind: kind.to_string(),
                    event_type: String::new(),
                    knot_index,
                    status,
                    indexed_at: now,
                    knot_timestamp: knot_ts,
                    payload_text: String::new(),
                    payload_size: 0,
                };
                event_type::enrich(&mut entry, None);
                entries.push(entry);
                self.seen.lock().insert(key);
            }
        } else {
            // Non-wallet kinds: index the genesis knot from the
            // descriptor. The RPC layer doesn't expose per-knot walks
            // for these kinds (yet — see honest caveat in lib.rs).
            let knot_id = desc
                .get("genesis_knot_id")
                .and_then(|v| v.as_str())
                .map(|s| {
                    if s.starts_with("0x") {
                        s.to_string()
                    } else {
                        format!("0x{s}")
                    }
                })
                .unwrap_or_default();
            if !knot_id.is_empty() {
                let key = (string_id.to_string(), knot_id.clone());
                if !self.seen.lock().contains(&key) {
                    let mut entry = KnotIndexEntry {
                        knot_id,
                        string_id: string_id.to_string(),
                        string_kind: kind.to_string(),
                        event_type: String::new(),
                        knot_index: 0,
                        status: "active".to_string(),
                        indexed_at: now,
                        knot_timestamp: desc
                            .get("created_at")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(last_anchored_at),
                        payload_text: String::new(),
                        payload_size: 0,
                    };
                    event_type::enrich(&mut entry, None);
                    entries.push(entry);
                    self.seen.lock().insert(key);
                } else {
                    skipped = skipped.saturating_add(1);
                }
            }
        }

        let indexed = entries.len() as u64;
        if !entries.is_empty() {
            let last_id = entries.last().map(|e| e.knot_id.clone());
            let last_string = entries.last().map(|e| e.string_id.clone());
            self.search
                .index_entries(&entries)
                .map_err(|e| anyhow::anyhow!("tantivy commit: {e}"))?;
            let mut m = self.metrics.write();
            m.indexed_count = m.indexed_count.saturating_add(indexed);
            m.last_indexed_knot_id = last_id;
            m.last_indexed_string_id = last_string;
            m.last_indexed_at = Some(chrono::Utc::now().timestamp());
        }
        Ok((indexed, skipped))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::SearchService;
    use serde_json::json;
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn fresh_search() -> Arc<SearchService> {
        let dir = tempfile::tempdir().unwrap().keep();
        Arc::new(SearchService::open_or_create(dir).unwrap())
    }

    fn build_indexer(rpc_url: String, search: Arc<SearchService>) -> Arc<Indexer> {
        let cfg = Arc::new(AgentConfig {
            rpc_url: rpc_url.clone(),
            poll_interval: Duration::from_millis(50),
            list_strings_limit: 50,
            max_knots_per_poll: 10_000,
            ..AgentConfig::default()
        });
        let rpc = Arc::new(RpcClient::new(rpc_url, Duration::from_secs(2)).unwrap());
        let metrics = Arc::new(RwLock::new(AgentMetrics::default()));
        Arc::new(Indexer::new(cfg, rpc, search, metrics))
    }

    fn rpc_response(method_name: &str, result: Value) -> ResponseTemplate {
        let _ = method_name;
        ResponseTemplate::new(200).set_body_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": result,
        }))
    }

    /// Mock that branches on the JSON-RPC method.
    async fn mock_node(server: &MockServer, n_strings: u32) {
        let global = json!({
            "total_strings": n_strings,
            "total_knots": (n_strings as u64) * 2,
            "by_kind": {"wallet": {"strings": n_strings, "knots": (n_strings as u64) * 2}},
            "invariant_holds": true,
        });
        let strings: Vec<Value> = (0..n_strings)
            .map(|i| {
                json!({
                    "kind": "wallet",
                    "string_id": format!("0x{:040x}", i + 1),
                    "genesis_knot_id": format!("g{:063x}", i + 1),
                    "head_knot_id": format!("h{:063x}", i + 1),
                    "knot_count": 2,
                    "total_size_bytes": 0,
                    "is_deleted": false,
                    "created_at": 1_700_000_000,
                    "last_anchored_at": 1_700_000_000 + i as i64,
                })
            })
            .collect();
        let list_resp = json!({
            "total": n_strings,
            "offset": 0,
            "limit": 200,
            "kind_filter": null,
            "strings": strings,
        });

        // The wiremock matcher API doesn't expose request-body parsing
        // ergonomically, so we mount one mock that picks the result by
        // inspecting the JSON-RPC method via a custom respond_with.
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(move |req: &wiremock::Request| {
                let body: Value = serde_json::from_slice(&req.body).unwrap_or_default();
                let m = body.get("method").and_then(|v| v.as_str()).unwrap_or("");
                let id = body.get("id").cloned().unwrap_or(json!(1));
                let result = match m {
                    "rope_globalStats" => global.clone(),
                    "rope_listStrings" => list_resp.clone(),
                    "rope_getStringWithKnots" => {
                        // Two knots per wallet: a genesis (active) and
                        // an append (active).
                        let owner = body
                            .get("params")
                            .and_then(|p| p.get(0))
                            .and_then(|v| v.as_str())
                            .unwrap_or("0xunknown");
                        json!({
                            "wallet_address": owner,
                            "string_id": format!("{owner}-string"),
                            "knots": [
                                {
                                    "knot_index": 0,
                                    "string_id": format!("{owner}-knot0"),
                                    "status": "active",
                                    "tombstone": null,
                                },
                                {
                                    "knot_index": 1,
                                    "string_id": format!("{owner}-knot1"),
                                    "status": "active",
                                    "tombstone": null,
                                },
                            ],
                            "knot_count": 2,
                            "active_count": 2,
                            "tombstone_count": 0,
                        })
                    }
                    "rope_appendToLedger" => {
                        json!({"index": 1, "hash": "0xanchor"})
                    }
                    _ => json!(null),
                };
                ResponseTemplate::new(200).set_body_json(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                }))
            })
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn poll_once_indexes_every_knot_returned_by_mock_rpc() {
        let server = MockServer::start().await;
        mock_node(&server, 50).await;
        let search = fresh_search();
        let indexer = build_indexer(server.uri(), search.clone());
        let outcome = indexer.poll_once().await.unwrap();
        // 50 wallets × 2 knots each = 100 knots indexed.
        assert_eq!(outcome.strings_walked, 50);
        assert_eq!(outcome.knots_indexed, 100);
        assert_eq!(search.doc_count(), 100);
    }

    #[tokio::test]
    async fn poll_once_is_idempotent_under_replay() {
        let server = MockServer::start().await;
        mock_node(&server, 5).await;
        let search = fresh_search();
        let indexer = build_indexer(server.uri(), search.clone());
        indexer.poll_once().await.unwrap();
        let first = search.doc_count();
        indexer.poll_once().await.unwrap();
        let second = search.doc_count();
        assert_eq!(first, 10);
        assert_eq!(second, 10, "replay must not duplicate documents");
    }

    #[tokio::test]
    async fn poll_once_propagates_rpc_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(rpc_response(
                "rope_globalStats",
                json!({"total_strings": 0, "total_knots": 0, "by_kind": {}, "invariant_holds": true}),
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // 2nd call (rope_listStrings) returns a JSON-RPC error.
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "error": {"code": -32601, "message": "list_strings exploded"},
            })))
            .mount(&server)
            .await;
        let search = fresh_search();
        let indexer = build_indexer(server.uri(), search);
        let err = indexer.poll_once().await.unwrap_err();
        assert!(format!("{err}").contains("list_strings exploded"));
    }
}
