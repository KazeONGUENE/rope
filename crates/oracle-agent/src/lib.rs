//! # `oracle-agent` — Datachain Rope canonical price-oracle testimony agent
//!
//! `OracleAgent` is one of the five canonical AI testimony agents listed by
//! the Datachain Rope explorer's `canonical_ai_agents()` (see
//! `crates/rope-explorer/src/main.rs` lines 4671–4682). Per that registry it
//! is responsible for:
//!
//! > Publishing DC FAT and stablecoin price testimonies sourced from DCSwap
//! > reserves and external feeds (XDCScan, GeckoTerminal).
//!
//! This crate makes that registry entry real: it pulls the canonical price
//! feed published by the DCSwap indexer at `https://dcswap.net/v1/prices`,
//! signs a structured [`OraclePriceTestimony`] with the agent's keypair, and
//! anchors it as a testimony knot on the agent's wallet string via
//! `rope_appendToLedger` on a local rope-node.
//!
//! The agent runs as a long-lived service. The control loop is in
//! [`OracleAgent::run`].
//!
//! ## Modules
//!
//! * [`config`] — typed configuration ([`AgentConfig`]).
//! * [`feeds`] — HTTP price feed client ([`feeds::PriceFeed`]).
//! * [`signer`] — `rope-crypto`-backed testimony signer
//!   ([`signer::TestimonySigner`]).
//! * [`anchor`] — JSON-RPC client for the local rope-node
//!   ([`anchor::AnchorClient`]).
//!
//! ## Wire shape
//!
//! Every cycle anchors one knot whose `description` is a short tag (so it
//! shows up nicely on DCScan) and whose `metadata` contains the full
//! [`OraclePriceTestimony`] serialised as JSON under the `payload` key, plus
//! the canonical signature fields from [`signer::SignedTestimony`]. The
//! agent's wallet (default `0x...C002`) is the on-chain identity DCScan uses
//! to filter testimonies on the `/agents` page.

pub mod anchor;
pub mod config;
pub mod feeds;
pub mod signer;

use std::sync::Arc;
use std::time::Duration;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

pub use crate::anchor::{AnchorClient, AnchorError, AppendResult, Interaction};
pub use crate::config::{AgentConfig, ConfigError, SigningMode};
pub use crate::feeds::{
    FeedError, PriceFeed, PriceMechanism, PriceSnapshot, PriceSource, TokenPrice,
};
pub use crate::signer::{SignedTestimony, SignerError, TestimonySigner};

/// Stable agent identifier — matches the `id` field returned by
/// `/api/v1/ai-agents` for the OracleAgent row.
pub const ORACLE_AGENT_ID: &str = "oracle";

/// Stable agent display name — matches the `name` field returned by
/// `/api/v1/ai-agents` for the OracleAgent row.
pub const ORACLE_AGENT_NAME: &str = "OracleAgent";

/// Mechanism schema version this agent emits in every testimony. Bump when
/// the testimony shape changes in a non-backward-compatible way.
pub const ORACLE_TESTIMONY_SCHEMA: &str = "oracle-price-testimony/v1";

/// The structured payload anchored on every cycle.
///
/// This is the type DCScan and any downstream verifier should decode. It
/// captures the full provenance of the price reading — every contributing
/// source, the mechanism version, and both the feed-side and agent-side
/// timestamps — so a replay/audit can reconstruct exactly what the agent
/// observed.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct OraclePriceTestimony {
    /// Schema version of this payload.
    pub schema: String,
    /// Stable agent id (`"oracle"`).
    pub agent_id: String,
    /// Display name (`"OracleAgent"`).
    pub agent_name: String,
    /// 0x-prefixed wallet hex this testimony is anchored on.
    pub agent_wallet: String,
    /// 32-byte Ed25519 public key, hex-encoded — useful as a stable agent
    /// identity even across wallet changes.
    pub agent_ed25519_pk_hex: String,
    /// Local timestamp (epoch seconds) at which the agent built this
    /// testimony.
    pub timestamp: i64,
    /// Server-side feed timestamp (epoch seconds) reported by dcswap.net.
    pub feed_timestamp: i64,
    /// URL the snapshot was sourced from.
    pub feed_url: String,
    /// Mechanism version (`"2.1"` post-2026-03-14 — see workspace rule
    /// `handover-canonical-fat-price-2026-03-14`).
    pub mechanism_version: String,
    /// Mechanism phase (`"market"` post-2026-03-14).
    pub mechanism_phase: String,
    /// Canonical FAT price USD.
    pub fat_price_usd: f64,
    /// 24h change reported by the feed.
    pub fat_change_24h: f64,
    /// Source breakdown that produced `fat_price_usd`.
    pub fat_source_label: String,
    /// USDC price (USD).
    pub usdc_price_usd: f64,
    /// USDT price (USD).
    pub usdt_price_usd: f64,
    /// EUROD price (USD).
    pub eurod_price_usd: f64,
    /// Per-source breakdown (e.g. dcswap-reserves at weight 0.7,
    /// geckoterminal-xdc at weight 0.3).
    pub sources: Vec<PriceSource>,
    /// Mechanism floor (P_floor) at the time of the reading. Should be 0.0
    /// post-2026-03-14 (the floor was permanently removed in v2.1).
    pub mechanism_floor: f64,
    /// Optional human-readable note from the feed.
    pub mechanism_note: Option<String>,
}

impl OraclePriceTestimony {
    /// Build a testimony from a fresh price snapshot, the agent's wallet,
    /// and the agent's Ed25519 public key.
    pub fn from_snapshot(
        snap: &PriceSnapshot,
        agent_wallet: &str,
        agent_ed25519_pk_hex: &str,
    ) -> Self {
        Self {
            schema: ORACLE_TESTIMONY_SCHEMA.to_string(),
            agent_id: ORACLE_AGENT_ID.to_string(),
            agent_name: ORACLE_AGENT_NAME.to_string(),
            agent_wallet: agent_wallet.to_string(),
            agent_ed25519_pk_hex: agent_ed25519_pk_hex.to_string(),
            timestamp: snap.fetched_at,
            feed_timestamp: snap.feed_timestamp,
            feed_url: snap.source_url.clone(),
            mechanism_version: snap.mechanism.version.clone(),
            mechanism_phase: snap.mechanism.phase.clone(),
            fat_price_usd: snap.fat.usd,
            fat_change_24h: snap.fat.change_24h,
            fat_source_label: snap.fat.source.clone(),
            usdc_price_usd: snap.usdc.usd,
            usdt_price_usd: snap.usdt.usd,
            eurod_price_usd: snap.eurod.usd,
            sources: snap.mechanism.sources.clone(),
            mechanism_floor: snap.mechanism.p_floor,
            mechanism_note: snap.mechanism.note.clone(),
        }
    }

    /// Canonical bytes the agent signs. We use `serde_json::to_vec` directly
    /// — the field ordering is deterministic because all fields are named in
    /// a `struct` (serde preserves declaration order), so two runs with the
    /// same input produce the same canonical bytes.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("OraclePriceTestimony must always serialize")
    }

    /// Short human-readable label used as the `description` field of the
    /// anchored interaction. DCScan shows this in `/testimonies`.
    pub fn description(&self) -> String {
        format!(
            "OracleAgent FAT={:.6} USD (mech={}, srcs={})",
            self.fat_price_usd,
            self.mechanism_version,
            self.sources.len()
        )
    }
}

/// Result of one cycle of the agent loop.
#[derive(Clone, Debug)]
pub struct CycleOutcome {
    pub started_at: i64,
    pub completed_at: i64,
    pub testimony: OraclePriceTestimony,
    pub signed: SignedTestimony,
    pub anchor: AppendResult,
}

/// Errors raised by [`OracleAgent::run_once`].
#[derive(Debug, thiserror::Error)]
pub enum CycleError {
    #[error("price feed failure: {0}")]
    Feed(#[from] FeedError),
    #[error("anchor failure: {0}")]
    Anchor(#[from] AnchorError),
}

/// Live agent metrics exposed for the CLI / Prometheus exporter.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AgentMetrics {
    /// Number of testimonies anchored since process start.
    pub anchor_count: u64,
    /// Number of failed cycles (feed or anchor) since process start.
    pub error_count: u64,
    /// Last successful anchor timestamp (epoch seconds), 0 if none yet.
    pub last_anchor_at: i64,
    /// Last knot string id anchored (hex), empty if none yet.
    pub last_knot_string_id: String,
    /// Last FAT price witnessed, 0.0 if none yet.
    pub last_fat_price_usd: f64,
}

/// The OracleAgent orchestrator. Owns the keypair, the HTTP clients, and the
/// metrics. Use [`OracleAgent::run`] to spin the control loop; in tests use
/// [`OracleAgent::run_once`] for a single iteration.
pub struct OracleAgent {
    config: AgentConfig,
    feed: PriceFeed,
    signer: TestimonySigner,
    anchor: AnchorClient,
    metrics: Arc<RwLock<AgentMetrics>>,
}

impl OracleAgent {
    /// Build an agent from a validated [`AgentConfig`] and an explicit
    /// [`TestimonySigner`]. This is the primary constructor used by the CLI
    /// and by tests.
    pub fn new(config: AgentConfig, signer: TestimonySigner) -> Result<Self, BuildError> {
        config.validate()?;
        let feed = PriceFeed::from_config(&config).map_err(BuildError::Feed)?;
        let anchor = AnchorClient::from_config(&config).map_err(BuildError::Anchor)?;
        Ok(Self {
            config,
            feed,
            signer,
            anchor,
            metrics: Arc::new(RwLock::new(AgentMetrics::default())),
        })
    }

    /// Test-only constructor that wires explicit feed + anchor clients
    /// (against a mock server). The signer is also passed in so the test can
    /// pin a deterministic seed.
    pub fn for_testing(
        config: AgentConfig,
        feed: PriceFeed,
        anchor: AnchorClient,
        signer: TestimonySigner,
    ) -> Self {
        Self {
            config,
            feed,
            signer,
            anchor,
            metrics: Arc::new(RwLock::new(AgentMetrics::default())),
        }
    }

    /// Read-only view of the agent's metrics (cheap clone — `AgentMetrics`
    /// is `Clone`).
    pub fn metrics(&self) -> AgentMetrics {
        self.metrics.read().clone()
    }

    /// Stable agent wallet (0x-prefixed hex).
    pub fn wallet_hex(&self) -> &str {
        &self.config.wallet_hex
    }

    /// Stable Ed25519 public key (hex). Useful for log lines and metrics.
    pub fn ed25519_pk_hex(&self) -> String {
        self.signer.ed25519_public_key_hex()
    }

    /// Optionally call `rope_createPersonalLedger` once before the loop
    /// starts. Returns `Ok(true)` iff the ledger was newly created.
    pub async fn ensure_ledger(&self) -> Result<bool, AnchorError> {
        if !self.config.auto_create_ledger {
            return Ok(false);
        }
        self.anchor.ensure_ledger(&self.config.wallet_hex).await
    }

    /// Run one cycle: fetch → build → sign → anchor. Returns the
    /// [`CycleOutcome`] on success.
    pub async fn run_once(&self) -> Result<CycleOutcome, CycleError> {
        let started_at = chrono::Utc::now().timestamp();
        let snap = self.feed.fetch_with_retry().await?;
        let testimony = OraclePriceTestimony::from_snapshot(
            &snap,
            &self.config.wallet_hex,
            &self.ed25519_pk_hex(),
        );
        let canonical = testimony.canonical_bytes();
        let signed = self.signer.sign(&canonical);

        let interaction = build_interaction(&testimony, &signed);
        let anchor = self
            .anchor
            .append_with_retry(&self.config.wallet_hex, &interaction)
            .await?;

        let completed_at = chrono::Utc::now().timestamp();
        {
            let mut m = self.metrics.write();
            m.anchor_count += 1;
            m.last_anchor_at = completed_at;
            m.last_knot_string_id = anchor.knot_string_id.clone();
            m.last_fat_price_usd = testimony.fat_price_usd;
        }
        Ok(CycleOutcome {
            started_at,
            completed_at,
            testimony,
            signed,
            anchor,
        })
    }

    /// Long-running control loop. Yields once per [`AgentConfig::interval`]
    /// and never returns under normal operation. Errors in a cycle are
    /// counted in metrics and logged but do NOT abort the loop — the agent
    /// is meant to keep trying.
    ///
    /// `cancel` is a future that, when ready, stops the loop and returns
    /// gracefully. Pass `std::future::pending::<()>()` for "run forever".
    pub async fn run<F: std::future::Future<Output = ()> + Unpin>(&self, mut cancel: F) {
        tracing::info!(
            target: "oracle_agent",
            schema = ORACLE_TESTIMONY_SCHEMA,
            wallet = self.config.wallet_hex.as_str(),
            agent_pk = %self.ed25519_pk_hex(),
            feed_url = self.config.feed_url.as_str(),
            rpc_url = self.config.rpc_url.as_str(),
            interval_secs = self.config.interval.as_secs(),
            signing_mode = %self.signer.mode(),
            "OracleAgent starting"
        );
        if let Err(e) = self.ensure_ledger().await {
            tracing::warn!(
                target: "oracle_agent",
                error = %e,
                "ensure_ledger failed; the first append may bootstrap it on the node"
            );
        }
        loop {
            let cycle_start = std::time::Instant::now();
            match self.run_once().await {
                Ok(out) => {
                    tracing::info!(
                        target: "oracle_agent",
                        knot_string_id = %out.anchor.knot_string_id,
                        fat_price_usd = out.testimony.fat_price_usd,
                        anchor_count = self.metrics().anchor_count,
                        "cycle ok"
                    );
                }
                Err(e) => {
                    {
                        let mut m = self.metrics.write();
                        m.error_count += 1;
                    }
                    tracing::error!(
                        target: "oracle_agent",
                        error = %e,
                        error_count = self.metrics().error_count,
                        "cycle failed; will retry next interval"
                    );
                }
            }
            let elapsed = cycle_start.elapsed();
            let sleep_for = self.config.interval.saturating_sub(elapsed);
            // If the cycle took longer than the interval, fire the next one
            // immediately rather than building up backlog.
            let sleep_for = if sleep_for == Duration::ZERO {
                Duration::from_millis(1)
            } else {
                sleep_for
            };
            tokio::select! {
                _ = tokio::time::sleep(sleep_for) => {}
                _ = &mut cancel => {
                    tracing::info!(target: "oracle_agent", "cancel requested; stopping loop");
                    return;
                }
            }
        }
    }
}

/// Errors emitted by [`OracleAgent::new`].
#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("invalid configuration: {0}")]
    Config(#[from] ConfigError),
    #[error("feed client error: {0}")]
    Feed(FeedError),
    #[error("anchor client error: {0}")]
    Anchor(AnchorError),
}

/// Build the [`Interaction`] sent over the wire.
///
/// `description` shows up in DCScan; `metadata` carries the structured
/// payload + signature so any verifier can re-check the testimony from the
/// chain alone.
fn build_interaction(testimony: &OraclePriceTestimony, signed: &SignedTestimony) -> Interaction {
    let payload_json =
        serde_json::to_string(testimony).expect("OraclePriceTestimony must serialize to JSON");
    let mut interaction = Interaction::testimony(testimony.description())
        .with_meta("schema", &testimony.schema)
        .with_meta("agent_id", &testimony.agent_id)
        .with_meta("agent_name", &testimony.agent_name)
        .with_meta("payload", payload_json)
        .with_meta("payload_hash", &signed.payload_hash)
        .with_meta("ed25519_pk", &signed.ed25519_public_key_hex)
        .with_meta("ed25519_sig", &signed.ed25519_signature_hex)
        .with_meta("signing_mode", &signed.signing_mode)
        .with_meta("fat_price_usd", format!("{:.10}", testimony.fat_price_usd))
        .with_meta("mechanism_version", &testimony.mechanism_version)
        .with_meta("feed_url", &testimony.feed_url)
        .with_meta("feed_timestamp", testimony.feed_timestamp.to_string());
    if !signed.dilithium_signature_hex.is_empty() {
        interaction = interaction
            .with_meta("dilithium_pk", &signed.dilithium_public_key_hex)
            .with_meta("dilithium_sig", &signed.dilithium_signature_hex);
    }
    interaction
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn canonical_feed_body() -> serde_json::Value {
        serde_json::json!({
            "success": true,
            "data": {
                "USDC": { "usd": 0.999967, "change_24h": 0.006, "source": "coingecko" },
                "USDT": { "usd": 1.0, "change_24h": -0.002, "source": "coingecko" },
                "EUROD": { "usd": 1.1447, "change_24h": 0, "source": "exchangerate-api" },
                "FAT": {
                    "usd": 0.007408,
                    "change_24h": -3.199,
                    "source": "reconciled(dcswap-reserves+geckoterminal-xdc)"
                }
            },
            "timestamp": 1773507597_i64,
            "priceMechanism": {
                "version": "2.1",
                "phase": "market",
                "price": 0.007408,
                "p_ref": 0.0025,
                "p_floor": 0.0,
                "sources": [
                    { "source": "dcswap-reserves", "price": 0.010297, "weight": 0.7 },
                    { "source": "geckoterminal-xdc", "price": 0.000667, "weight": 0.3 }
                ],
                "note": "price = VWAP(sources). Pure market price — no artificial floor."
            }
        })
    }

    fn ok_anchor_response(knot_id: &str) -> serde_json::Value {
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": { "index": 1, "hash": knot_id }
        })
    }

    #[test]
    fn canonical_bytes_round_trip_through_serde() {
        let snap = PriceSnapshot {
            fat: TokenPrice {
                usd: 0.0123,
                change_24h: 1.5,
                source: "reconciled".into(),
            },
            usdc: TokenPrice {
                usd: 1.0,
                change_24h: 0.0,
                source: "coingecko".into(),
            },
            usdt: TokenPrice::default(),
            eurod: TokenPrice::default(),
            mechanism: PriceMechanism {
                version: "2.1".into(),
                phase: "market".into(),
                price: 0.0123,
                p_ref: 0.0,
                p_floor: 0.0,
                sources: vec![PriceSource {
                    source: "dcswap-reserves".into(),
                    price: 0.0123,
                    weight: 1.0,
                }],
                note: None,
            },
            feed_timestamp: 1_700_000_000,
            fetched_at: 1_700_000_001,
            source_url: "http://example/v1/prices".into(),
        };
        let testimony = OraclePriceTestimony::from_snapshot(
            &snap,
            "0x0000000000000000000000000000000000000C002",
            "deadbeef",
        );
        let bytes = testimony.canonical_bytes();
        let decoded: OraclePriceTestimony = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(testimony, decoded);
        // Re-encoding must be deterministic so that the BLAKE3 hash of
        // canonical bytes is reproducible across the loop.
        assert_eq!(decoded.canonical_bytes(), bytes);
    }

    #[test]
    fn description_mentions_fat_price_and_mechanism_version() {
        let snap = PriceSnapshot {
            fat: TokenPrice {
                usd: 0.0123,
                change_24h: 1.5,
                source: "reconciled".into(),
            },
            mechanism: PriceMechanism {
                version: "2.1".into(),
                phase: "market".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let testimony = OraclePriceTestimony::from_snapshot(
            &snap,
            "0x0000000000000000000000000000000000000C002",
            "deadbeef",
        );
        let d = testimony.description();
        assert!(d.contains("0.012300"));
        assert!(d.contains("mech=2.1"));
    }

    #[tokio::test]
    async fn run_once_end_to_end_against_mocks() {
        let feed_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(canonical_feed_body()))
            .mount(&feed_server)
            .await;

        let rpc_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_anchor_response("0xfeedface")),
            )
            .mount(&rpc_server)
            .await;

        let config = AgentConfig {
            feed_url: format!("{}/v1/prices", feed_server.uri()),
            rpc_url: rpc_server.uri(),
            interval: Duration::from_millis(50),
            auto_create_ledger: false, // skip the ensure_ledger call
            ..AgentConfig::default()
        };

        let signer = TestimonySigner::from_seed_bytes([42u8; 32], SigningMode::Ed25519Only);
        let feed = PriceFeed::from_config(&config).unwrap();
        let anchor = AnchorClient::from_config(&config).unwrap();
        let agent = OracleAgent::for_testing(config, feed, anchor, signer);

        let outcome = agent.run_once().await.expect("run_once must succeed");
        assert_eq!(outcome.anchor.knot_string_id, "0xfeedface");
        assert_eq!(outcome.testimony.mechanism_version, "2.1");
        assert!((outcome.testimony.fat_price_usd - 0.007408).abs() < 1e-9);
        assert!(outcome.signed.payload_hash_matches());
        assert_eq!(outcome.signed.signing_mode, "ed25519-only");

        let m = agent.metrics();
        assert_eq!(m.anchor_count, 1);
        assert_eq!(m.last_knot_string_id, "0xfeedface");
        assert!(m.last_fat_price_usd > 0.0);
    }

    #[tokio::test]
    async fn run_once_increments_error_count_on_feed_failure() {
        let feed_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .mount(&feed_server)
            .await;

        let rpc_server = MockServer::start().await;
        // Anchor must NOT be reached if the feed fails — we don't register a
        // mock so any POST would 404.

        let config = AgentConfig {
            feed_url: format!("{}/v1/prices", feed_server.uri()),
            rpc_url: rpc_server.uri(),
            interval: Duration::from_millis(50),
            max_retries: 0, // fail fast in tests
            auto_create_ledger: false,
            ..AgentConfig::default()
        };

        let signer = TestimonySigner::ephemeral(SigningMode::Ed25519Only);
        let feed = PriceFeed::from_config(&config).unwrap();
        let anchor = AnchorClient::from_config(&config).unwrap();
        let agent = OracleAgent::for_testing(config, feed, anchor, signer);

        let err = agent
            .run_once()
            .await
            .expect_err("feed failure must surface");
        assert!(matches!(err, CycleError::Feed(_)));
    }

    #[tokio::test]
    async fn run_loop_can_be_cancelled() {
        let feed_server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/prices"))
            .respond_with(ResponseTemplate::new(200).set_body_json(canonical_feed_body()))
            .mount(&feed_server)
            .await;

        let rpc_server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(ok_anchor_response("0xfeedface")),
            )
            .mount(&rpc_server)
            .await;

        let config = AgentConfig {
            feed_url: format!("{}/v1/prices", feed_server.uri()),
            rpc_url: rpc_server.uri(),
            interval: Duration::from_millis(20),
            auto_create_ledger: false,
            ..AgentConfig::default()
        };

        let signer = TestimonySigner::ephemeral(SigningMode::Ed25519Only);
        let feed = PriceFeed::from_config(&config).unwrap();
        let anchor = AnchorClient::from_config(&config).unwrap();
        let agent = OracleAgent::for_testing(config, feed, anchor, signer);

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let agent = Arc::new(agent);
        let agent_handle = agent.clone();
        let join = tokio::spawn(async move {
            agent_handle
                .run(Box::pin(async move {
                    let _ = rx.await;
                }))
                .await;
        });

        // Let at least one cycle complete, then cancel.
        tokio::time::sleep(Duration::from_millis(120)).await;
        tx.send(()).expect("cancel must send");
        tokio::time::timeout(Duration::from_secs(5), join)
            .await
            .expect("loop must stop within 5s after cancel")
            .expect("join must succeed");

        let m = agent.metrics();
        assert!(
            m.anchor_count >= 1,
            "expected at least one successful cycle before cancel, got {}",
            m.anchor_count
        );
    }
}
