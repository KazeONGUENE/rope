//! Source-of-knots abstraction.
//!
//! The agent's control loop calls [`KnotSubscriber::next_batch`] every
//! poll tick. Production uses [`RpcPollSubscriber`] which keeps a
//! cursor of the last seen cord-anchor index and asks the local
//! rope-node for the slice `(last, head]` capped at the configured
//! per-tick batch size. Tests can implement the trait directly.
//!
//! The trait is deliberately narrow — knot fetching, decoding, and
//! the cursor are all hidden behind it so the agent core only sees a
//! `Vec<Knot>` per tick.

use std::sync::Arc;

use async_trait::async_trait;

use crate::knot::{Knot, KnotSource};
use crate::rpc::{JsonRpcError, RopeRpcClient};

/// Trait implemented by anything that can produce a batch of knots
/// for the agent to validate.
#[async_trait]
pub trait KnotSubscriber: Send + Sync + std::fmt::Debug {
    /// Fetch up to `max` new knots since the last call. Implementors
    /// are responsible for advancing their internal cursor exactly
    /// once per successful call.
    async fn next_batch(&self, max: u64) -> Result<Vec<Knot>, JsonRpcError>;
}

/// JSON-RPC polling subscriber over the canonical
/// `rope_knotIndex` / `rope_getKnotByIndex` pair.
#[derive(Debug)]
pub struct RpcPollSubscriber<C: RopeRpcClient + ?Sized> {
    client: Arc<C>,
    cursor: parking_lot::Mutex<u64>,
    /// When `false`, the subscriber additionally enumerates entity
    /// strings via `rope_listStrings` (kind=wallet). v0.1: the
    /// extension is a stubbed warning until the lattice walk is
    /// stabilized — see the crate-level scope note. Setting this to
    /// `false` only logs an informational warning today; it does not
    /// crash and does not change the returned batch.
    anchor_only: bool,
}

impl<C: RopeRpcClient + ?Sized> RpcPollSubscriber<C> {
    /// Construct a subscriber rooted at cord index 0 (i.e. genesis).
    /// The first call to `next_batch` will return everything from 1
    /// up to the current head, capped at `max`.
    pub fn new(client: Arc<C>, anchor_only: bool) -> Self {
        Self {
            client,
            cursor: parking_lot::Mutex::new(0),
            anchor_only,
        }
    }

    /// Construct a subscriber that starts from a specific cursor.
    /// Useful for an agent that crash-recovers and remembers its last
    /// witnessed knot index.
    pub fn with_cursor(client: Arc<C>, cursor: u64, anchor_only: bool) -> Self {
        Self {
            client,
            cursor: parking_lot::Mutex::new(cursor),
            anchor_only,
        }
    }

    /// Read the current cursor (test-only).
    #[cfg(test)]
    pub(crate) fn cursor(&self) -> u64 {
        *self.cursor.lock()
    }

    /// Decode an EVM-shape knot JSON body into a [`Knot`]. We extract
    /// the block hash as the canonical knot id and the block hash
    /// bytes as the signing message placeholder. Today the EVM-shape
    /// cord anchor does NOT carry a `HybridSignature`; the verifier
    /// will mark it `Skipped`. When Phase 2 lights up real consensus
    /// signatures, this is where the hybrid sig material will be
    /// extracted from the knot extra-data and attached.
    fn decode_anchor(value: &serde_json::Value, index: u64) -> Option<Knot> {
        let hash = value.get("hash").and_then(|v| v.as_str())?;
        // Best-effort signing-message preimage: the anchor hash bytes
        // themselves. Until consensus is enabled, this is what the
        // verifier sees; in the absence of signature material it
        // simply Skips. The placeholder is documented in the verify
        // module.
        let signing_message = hex::decode(hash.trim_start_matches("0x")).unwrap_or_default();
        Some(Knot::new(
            hash,
            index,
            KnotSource::CordAnchor,
            signing_message,
        ))
    }
}

#[async_trait]
impl<C: RopeRpcClient + ?Sized> KnotSubscriber for RpcPollSubscriber<C> {
    async fn next_batch(&self, max: u64) -> Result<Vec<Knot>, JsonRpcError> {
        if !self.anchor_only {
            tracing::warn!(
                target: "validation_agent::subscriber",
                "anchor_only=false: per-entity-string scanning is not yet implemented in v0.1; \
                 falling back to anchor-only behaviour. See crate-level scope note.",
            );
        }

        let head = self.client.knot_index().await?;

        // Read + reserve the window under a short lock — drop the
        // guard BEFORE any `.await` so the future stays `Send`.
        let (start, end, want) = {
            let mut cursor = self.cursor.lock();
            if head <= *cursor {
                tracing::trace!(
                    target: "validation_agent::subscriber",
                    head = head,
                    cursor = *cursor,
                    "no new cord anchor knots since last tick",
                );
                return Ok(Vec::new());
            }
            let want = head.saturating_sub(*cursor).min(max);
            let start = *cursor + 1;
            let end = start + want; // exclusive upper bound
                                    // Pre-advance the cursor: even if a knot fetch fails we
                                    // do NOT want to spin forever on the same bad index, and
                                    // re-entry from another tick must not double-issue the
                                    // same window.
            *cursor = end - 1;
            (start, end, want)
        };

        let mut out = Vec::with_capacity(want as usize);
        for idx in start..end {
            match self.client.get_knot_by_index(idx).await {
                Ok(value) if value.is_null() => {
                    tracing::debug!(
                        target: "validation_agent::subscriber",
                        index = idx,
                        "node returned null knot — skipping (chain reorg or pruning)",
                    );
                }
                Ok(value) => {
                    if let Some(knot) = Self::decode_anchor(&value, idx) {
                        out.push(knot);
                    } else {
                        tracing::debug!(
                            target: "validation_agent::subscriber",
                            index = idx,
                            "knot body missing `hash` field — skipping",
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        target: "validation_agent::subscriber",
                        index = idx,
                        error = %e,
                        "skipping knot due to RPC error",
                    );
                }
            }
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rpc::mock::MockRpcClient;
    use serde_json::json;

    fn mk_anchor(hash: &str) -> serde_json::Value {
        json!({
            "hash": hash,
            "number": "0x1",
            "miner": "0x000000000000000000000000000000000000C001",
        })
    }

    #[tokio::test]
    async fn empty_chain_yields_empty_batch() {
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok("rope_knotIndex", json!("0x0"));
        let sub = RpcPollSubscriber::new(mock.clone() as Arc<dyn RopeRpcClient>, true);
        let batch = sub.next_batch(64).await.unwrap();
        assert!(batch.is_empty());
        assert_eq!(sub.cursor(), 0);
    }

    #[tokio::test]
    async fn polls_three_knots_and_advances_cursor() {
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok("rope_knotIndex", json!("0x3"));
        mock.enqueue_ok("rope_getKnotByIndex", mk_anchor("0xaa01"));
        mock.enqueue_ok("rope_getKnotByIndex", mk_anchor("0xaa02"));
        mock.enqueue_ok("rope_getKnotByIndex", mk_anchor("0xaa03"));

        let sub = RpcPollSubscriber::new(mock.clone() as Arc<dyn RopeRpcClient>, true);
        let batch = sub.next_batch(64).await.unwrap();
        assert_eq!(batch.len(), 3);
        assert_eq!(batch[0].knot_id, "0xaa01");
        assert_eq!(batch[2].knot_id, "0xaa03");
        assert_eq!(sub.cursor(), 3);
    }

    #[tokio::test]
    async fn respects_max_per_tick_cap() {
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok("rope_knotIndex", json!("0xff"));
        for _ in 0..2 {
            mock.enqueue_ok("rope_getKnotByIndex", mk_anchor("0xaa"));
        }
        let sub = RpcPollSubscriber::new(mock.clone() as Arc<dyn RopeRpcClient>, true);
        let batch = sub.next_batch(2).await.unwrap();
        assert_eq!(batch.len(), 2);
        assert_eq!(sub.cursor(), 2);
    }

    #[tokio::test]
    async fn skips_null_knot_responses() {
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok("rope_knotIndex", json!("0x2"));
        mock.enqueue_ok("rope_getKnotByIndex", json!(null));
        mock.enqueue_ok("rope_getKnotByIndex", mk_anchor("0xbb"));

        let sub = RpcPollSubscriber::new(mock.clone() as Arc<dyn RopeRpcClient>, true);
        let batch = sub.next_batch(8).await.unwrap();
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].knot_id, "0xbb");
        assert_eq!(sub.cursor(), 2);
    }

    #[tokio::test]
    async fn anchor_only_false_logs_warning_but_proceeds() {
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok("rope_knotIndex", json!("0x1"));
        mock.enqueue_ok("rope_getKnotByIndex", mk_anchor("0xcc"));
        let sub = RpcPollSubscriber::new(mock.clone() as Arc<dyn RopeRpcClient>, false);
        let batch = sub.next_batch(8).await.unwrap();
        assert_eq!(batch.len(), 1);
    }
}
