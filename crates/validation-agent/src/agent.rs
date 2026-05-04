//! Long-running orchestration loop.
//!
//! Wires the [`subscriber`] → [`verify`] → [`witness`] pipeline
//! together and exposes a small, lock-free metrics surface.
//!
//! The intended consumer is the binary in `main.rs`, but the agent
//! is fully driveable from a unit test against a mock RPC client —
//! see the integration test below.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rope_crypto::hybrid::HybridSigner;
use serde::Serialize;

use crate::config::ValidationAgentConfig;
use crate::knot::Knot;
use crate::rpc::{HttpRpcClient, RopeRpcClient};
use crate::subscriber::{KnotSubscriber, RpcPollSubscriber};
use crate::verify::{KnotVerifier, VerificationOutcome, VerificationResult};
use crate::witness::WitnessSubmitter;

/// Lock-free metrics counters that are safe to read from a separate
/// thread (e.g. a Prometheus exporter, a `/healthz` endpoint).
#[derive(Debug, Default)]
pub struct ValidationMetrics {
    /// Knots whose signature verified successfully.
    pub validated_count: AtomicU64,
    /// Knots whose signature was present but FAILED verification.
    pub rejected_count: AtomicU64,
    /// Knots that carried no signature material — neither valid nor
    /// rejected, just nothing to validate (the bulk of cord anchor
    /// knots until consensus signing is enabled).
    pub skipped_count: AtomicU64,
    /// Testimonies successfully submitted on the agent's wallet.
    pub testimonies_submitted: AtomicU64,
    /// Testimony submissions that returned an RPC error (we still
    /// count the underlying knot as `validated`).
    pub testimonies_failed: AtomicU64,
    /// Polling ticks completed.
    pub ticks_completed: AtomicU64,
    /// Unix-second timestamp of the most recent successful
    /// validation, or 0 if none yet.
    pub last_validation_at: AtomicI64,
}

impl ValidationMetrics {
    /// Snapshot the metrics into a serde-friendly struct (used by
    /// the binary's `--single-tick` summary printer and tests).
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            validated_count: self.validated_count.load(Ordering::Relaxed),
            rejected_count: self.rejected_count.load(Ordering::Relaxed),
            skipped_count: self.skipped_count.load(Ordering::Relaxed),
            testimonies_submitted: self.testimonies_submitted.load(Ordering::Relaxed),
            testimonies_failed: self.testimonies_failed.load(Ordering::Relaxed),
            ticks_completed: self.ticks_completed.load(Ordering::Relaxed),
            last_validation_at: self.last_validation_at.load(Ordering::Relaxed),
        }
    }
}

/// Plain serializable view of [`ValidationMetrics`].
#[derive(Debug, Clone, Serialize)]
pub struct MetricsSnapshot {
    /// Knots whose signature verified successfully.
    pub validated_count: u64,
    /// Knots whose signature was present but FAILED verification.
    pub rejected_count: u64,
    /// Knots whose signature material was absent.
    pub skipped_count: u64,
    /// Testimonies successfully submitted on the agent's wallet.
    pub testimonies_submitted: u64,
    /// Testimony submissions that returned an RPC error.
    pub testimonies_failed: u64,
    /// Polling ticks completed.
    pub ticks_completed: u64,
    /// Unix-second timestamp of the most recent successful validation.
    pub last_validation_at: i64,
}

/// The long-running ValidationAgent service.
#[derive(Debug)]
pub struct ValidationAgent {
    config: ValidationAgentConfig,
    subscriber: Arc<dyn KnotSubscriber>,
    verifier: KnotVerifier,
    submitter: Arc<WitnessSubmitter<dyn RopeRpcClient>>,
    metrics: Arc<ValidationMetrics>,
}

impl ValidationAgent {
    /// Build an agent from a config, an explicit RPC client, and an
    /// explicit hybrid signer. This is the lowest-level constructor
    /// and is what the integration tests use.
    pub fn new(
        config: ValidationAgentConfig,
        rpc: Arc<dyn RopeRpcClient>,
        signer: Arc<HybridSigner>,
    ) -> Self {
        let subscriber: Arc<dyn KnotSubscriber> =
            Arc::new(RpcPollSubscriber::new(rpc.clone(), config.anchor_only));
        let submitter: Arc<WitnessSubmitter<dyn RopeRpcClient>> = Arc::new(WitnessSubmitter::new(
            rpc.clone(),
            signer,
            config.wallet_address.clone(),
        ));
        Self {
            config,
            subscriber,
            verifier: KnotVerifier::new(),
            submitter,
            metrics: Arc::new(ValidationMetrics::default()),
        }
    }

    /// Convenience constructor that builds the production HTTP RPC
    /// client from the config and an in-memory ephemeral signer.
    pub async fn with_default_signer(config: ValidationAgentConfig) -> anyhow::Result<Self> {
        let rpc = Arc::new(HttpRpcClient::new(
            config.rpc_url.clone(),
            config.rpc_timeout,
        )?);
        let (signer, _pk) = HybridSigner::generate();
        Ok(Self::new(config, rpc, Arc::new(signer)))
    }

    /// Read-only metrics handle. Cheap to clone (it's an `Arc`).
    pub fn metrics(&self) -> Arc<ValidationMetrics> {
        Arc::clone(&self.metrics)
    }

    /// Borrow the active config (read-only).
    pub fn config(&self) -> &ValidationAgentConfig {
        &self.config
    }

    /// Run a single tick: pull a batch of knots, verify each, witness
    /// the valid ones. Returns the snapshot AFTER the tick.
    pub async fn tick(&self) -> anyhow::Result<MetricsSnapshot> {
        let max = self.config.max_knots_per_tick;
        let batch = self
            .subscriber
            .next_batch(max)
            .await
            .map_err(|e| anyhow::anyhow!("subscriber error: {e}"))?;
        let batch_len = batch.len();
        tracing::debug!(
            target: "validation_agent::agent",
            batch_size = batch_len,
            "verifying batch",
        );

        for knot in batch {
            self.process_knot(&knot).await;
        }

        self.metrics.ticks_completed.fetch_add(1, Ordering::Relaxed);
        Ok(self.metrics.snapshot())
    }

    async fn process_knot(&self, knot: &Knot) {
        let result: VerificationResult = self.verifier.verify(knot);
        match result.outcome {
            VerificationOutcome::Valid => {
                self.metrics.validated_count.fetch_add(1, Ordering::Relaxed);
                self.metrics
                    .last_validation_at
                    .store(result.validated_at, Ordering::Relaxed);
                if let Err(e) = self.submitter.submit(knot, &result).await {
                    self.metrics
                        .testimonies_failed
                        .fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        target: "validation_agent::agent",
                        knot_id = %knot.knot_id,
                        error = %e,
                        "testimony submission failed",
                    );
                } else {
                    self.metrics
                        .testimonies_submitted
                        .fetch_add(1, Ordering::Relaxed);
                }
            }
            VerificationOutcome::Invalid => {
                self.metrics.rejected_count.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    target: "validation_agent::agent",
                    knot_id = %knot.knot_id,
                    sig_algo = %result.sig_algo.as_str(),
                    note = result.note.as_deref().unwrap_or(""),
                    "rejected knot — signature did not verify; NOT emitting testimony",
                );
            }
            VerificationOutcome::Skipped => {
                self.metrics.skipped_count.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Run the long-lived poll loop. Honors `config.single_tick` for
    /// CI smoke tests. Cancellation is handled by the caller (drop
    /// the future, or wrap in a `tokio::select!` against a shutdown
    /// signal).
    pub async fn run(&self) -> anyhow::Result<()> {
        tracing::info!(
            target: "validation_agent::agent",
            rpc = %self.config.rpc_url,
            poll_interval_secs = self.config.poll_interval.as_secs(),
            wallet = %self.config.wallet_address,
            anchor_only = self.config.anchor_only,
            "ValidationAgent v{} starting", crate::VALIDATION_AGENT_VERSION,
        );

        if self.config.single_tick {
            let snap = self.tick().await?;
            tracing::info!(
                target: "validation_agent::agent",
                snapshot = ?snap,
                "single-tick mode: exiting after one tick",
            );
            return Ok(());
        }

        let interval = self.config.poll_interval.max(Duration::from_millis(50));
        let mut ticker = tokio::time::interval(interval);
        // The first tick fires immediately — that's fine, we're not
        // a periodic precise scheduler.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;
            match self.tick().await {
                Ok(snap) => {
                    tracing::trace!(
                        target: "validation_agent::agent",
                        snapshot = ?snap,
                        "tick complete",
                    );
                }
                Err(e) => {
                    // Don't let a transient RPC error kill the loop.
                    tracing::warn!(
                        target: "validation_agent::agent",
                        error = %e,
                        "tick failed — backing off until next interval",
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knot::{Knot, KnotSource};
    use crate::rpc::mock::MockRpcClient;
    use rope_crypto::hybrid::HybridSigner;
    use serde_json::json;

    fn anchor_json(hash: &str) -> serde_json::Value {
        json!({
            "hash": hash,
            "number": "0x1",
            "miner": "0x0000000000000000000000000000000000000001",
        })
    }

    #[tokio::test]
    async fn unsigned_anchor_is_skipped_not_rejected() {
        // EVM anchor knots today carry no HybridSignature — verify
        // the agent counts them as `skipped` and does NOT submit a
        // testimony.
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok("rope_knotIndex", json!("0x1"));
        mock.enqueue_ok("rope_getKnotByIndex", anchor_json("0xaa"));

        let (signer, _) = HybridSigner::generate();
        let cfg = ValidationAgentConfig::for_test();
        let agent = ValidationAgent::new(
            cfg,
            mock.clone() as Arc<dyn RopeRpcClient>,
            Arc::new(signer),
        );
        let snap = agent.tick().await.unwrap();
        assert_eq!(snap.skipped_count, 1);
        assert_eq!(snap.validated_count, 0);
        assert_eq!(snap.rejected_count, 0);
        assert_eq!(snap.testimonies_submitted, 0);
        assert_eq!(mock.count("rope_appendToLedger"), 0);
    }

    /// End-to-end mocked: feed a verifiable hybrid-signed knot
    /// directly through the agent (bypassing the subscriber via a
    /// custom `KnotSubscriber` mock). We exercise:
    ///   subscriber → verifier → witness → RPC
    #[tokio::test]
    async fn end_to_end_signed_knot_is_validated_and_witnessed() {
        // 1. Build a knot signed by a known signer S.
        let (s, pk) = HybridSigner::generate();
        let message = b"signed-anchor-payload".to_vec();
        let signature = s.sign(&message);
        let signed = Knot::new("0xfacefeed", 42, KnotSource::CordAnchor, message)
            .with_signature(pk, signature);

        // 2. Subscriber that yields exactly that knot once.
        #[derive(Debug)]
        struct OneShotSubscriber {
            inner: parking_lot::Mutex<Option<Knot>>,
        }
        #[async_trait::async_trait]
        impl KnotSubscriber for OneShotSubscriber {
            async fn next_batch(&self, _max: u64) -> Result<Vec<Knot>, crate::rpc::JsonRpcError> {
                if let Some(k) = self.inner.lock().take() {
                    Ok(vec![k])
                } else {
                    Ok(Vec::new())
                }
            }
        }

        // 3. Mock RPC ready to accept the testimony submission.
        let mock = Arc::new(MockRpcClient::new());
        mock.enqueue_ok(
            "rope_appendToLedger",
            json!({"index": 1, "hash": "0xtestimony1"}),
        );

        // 4. Build the agent with the custom subscriber.
        let (agent_signer, _) = HybridSigner::generate();
        let cfg = ValidationAgentConfig::for_test();
        let mut agent = ValidationAgent::new(
            cfg,
            mock.clone() as Arc<dyn RopeRpcClient>,
            Arc::new(agent_signer),
        );
        agent.subscriber = Arc::new(OneShotSubscriber {
            inner: parking_lot::Mutex::new(Some(signed)),
        });

        // 5. Drive one tick. Verify metrics are updated AND the
        //    rope_appendToLedger RPC was called exactly once with
        //    the canonical wallet.
        let snap = agent.tick().await.unwrap();
        assert_eq!(snap.validated_count, 1);
        assert_eq!(snap.rejected_count, 0);
        assert_eq!(snap.skipped_count, 0);
        assert_eq!(snap.testimonies_submitted, 1);
        assert_eq!(snap.testimonies_failed, 0);
        assert_eq!(snap.ticks_completed, 1);
        assert!(snap.last_validation_at > 0);
        assert_eq!(mock.count("rope_appendToLedger"), 1);

        let calls = mock.calls();
        let append_call = calls
            .iter()
            .find(|c| c.method == "rope_appendToLedger")
            .unwrap();
        assert_eq!(
            append_call.params.get(0).and_then(|v| v.as_str()),
            Some(crate::VALIDATION_AGENT_WALLET)
        );

        // 6. Second tick MUST be a no-op (subscriber returns empty).
        let snap2 = agent.tick().await.unwrap();
        assert_eq!(snap2.validated_count, 1);
        assert_eq!(snap2.testimonies_submitted, 1);
        assert_eq!(snap2.ticks_completed, 2);
    }

    #[tokio::test]
    async fn invalid_signature_is_counted_as_rejected_and_no_testimony_submitted() {
        // Signed by signer A, verified against signer B's pubkey.
        let (a, _pk_a) = HybridSigner::generate();
        let (_b, pk_b) = HybridSigner::generate();
        let message = b"will fail verification".to_vec();
        let sig = a.sign(&message);
        let bad_knot =
            Knot::new("0xbad", 1, KnotSource::CordAnchor, message).with_signature(pk_b, sig);

        #[derive(Debug)]
        struct OneShot {
            inner: parking_lot::Mutex<Option<Knot>>,
        }
        #[async_trait::async_trait]
        impl KnotSubscriber for OneShot {
            async fn next_batch(&self, _max: u64) -> Result<Vec<Knot>, crate::rpc::JsonRpcError> {
                Ok(self.inner.lock().take().into_iter().collect())
            }
        }

        let mock = Arc::new(MockRpcClient::new());
        let (agent_signer, _) = HybridSigner::generate();
        let mut agent = ValidationAgent::new(
            ValidationAgentConfig::for_test(),
            mock.clone() as Arc<dyn RopeRpcClient>,
            Arc::new(agent_signer),
        );
        agent.subscriber = Arc::new(OneShot {
            inner: parking_lot::Mutex::new(Some(bad_knot)),
        });

        let snap = agent.tick().await.unwrap();
        assert_eq!(snap.rejected_count, 1);
        assert_eq!(snap.validated_count, 0);
        assert_eq!(snap.testimonies_submitted, 0);
        assert_eq!(mock.count("rope_appendToLedger"), 0);
    }
}
