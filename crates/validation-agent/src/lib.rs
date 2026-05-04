// =============================================================================
// validation-agent — Datachain Rope canonical AI testimony agent
// =============================================================================
// Author: Kazé A. ONGUENE — Datachain Foundation
// =============================================================================
//
// One of the five canonical AI testimony agents exposed at
// `/api/v1/ai-agents` by the Rope explorer (DCScan). The ValidationAgent:
//
//   1. Subscribes to (or polls for) new cord anchor knots produced by the
//      local rope-node JSON-RPC.
//   2. For each knot, runs post-quantum signature verification — primary
//      path is hybrid ML-DSA-65 (CRYSTALS-Dilithium3) + Ed25519 via the
//      shared `rope-crypto` `HybridVerifier`. Knots that present only an
//      Ed25519 signature (no Dilithium public key) are accepted via the
//      classical fallback path with a downgrade warning.
//   3. For each successfully-validated cord anchor knot, emits a signed
//      `ValidationTestimony` knot via `rope_appendToLedger` on the agent's
//      own canonical wallet (`0x0000…C004`). The testimony is signed by the
//      agent's hybrid key and carries the verified knot id, signature
//      algorithm, witness timestamp, and validation latency.
//
// SCOPE NOTE — production readiness (v0.1):
//
//   * Uses RPC polling (HTTP). The websocket upgrade path (`wss://ws.…`) is
//     out of scope for this iteration; see `subscriber.rs` for the trait
//     boundary that admits a future `WsSubscriber` impl.
//   * EVM-shape cord anchor knots produced by Reth today do NOT carry a
//     `HybridSignature` (see Quipu Canon v2.0 Phase 2 — "real consensus
//     turned on with batched / aggregated signatures"). Until consensus is
//     enabled, anchor knots scanned by this agent enter the `skipped`
//     bucket (no signature material to verify) and do NOT produce a
//     testimony. The verification + witnessing code paths are themselves
//     real; the test suite exercises them end-to-end against synthesized
//     hybrid-signed payloads. When `verify_signatures` is flipped on in
//     `consensus_orchestrator.rs`, the same code becomes load-bearing
//     without modification. This trade-off is documented in the final
//     handover report rather than papered over with a fake "validated"
//     count.

#![deny(missing_docs)]
#![deny(unused_must_use)]
#![warn(rust_2018_idioms)]

//! ValidationAgent — Datachain Rope canonical AI testimony agent.
//!
//! See the crate-level source comment for the full specification. The
//! public surface is intentionally small:
//!
//! * [`config::ValidationAgentConfig`] — typed configuration loaded from
//!   the CLI or programmatically.
//! * [`verify::KnotVerifier`] / [`verify::VerificationResult`] —
//!   stateless post-quantum signature verifier built on top of
//!   `rope_crypto::HybridVerifier`.
//! * [`subscriber::KnotSubscriber`] — abstracts the source of new cord
//!   anchor knots behind a trait so the same control loop can talk to
//!   the production RPC, a WebSocket, or a unit-test fixture.
//! * [`witness::ValidationTestimony`] — the canonical testimony shape
//!   the agent emits.
//! * [`agent::ValidationAgent`] / [`agent::ValidationMetrics`] — the
//!   long-running service object that drives the poll-verify-witness
//!   loop.
//!
//! Quick start (programmatic):
//!
//! ```no_run
//! use validation_agent::{
//!     agent::ValidationAgent,
//!     config::ValidationAgentConfig,
//! };
//!
//! # async fn run() -> anyhow::Result<()> {
//! let cfg = ValidationAgentConfig::default();
//! let agent = ValidationAgent::with_default_signer(cfg).await?;
//! agent.run().await?;
//! # Ok(()) }
//! ```

pub mod agent;
pub mod config;
pub mod knot;
pub mod rpc;
pub mod subscriber;
pub mod verify;
pub mod witness;

pub use agent::{ValidationAgent, ValidationMetrics};
pub use config::ValidationAgentConfig;
pub use knot::{Knot, KnotId, KnotSource};
pub use rpc::{JsonRpcError, RopeRpcClient};
pub use subscriber::{KnotSubscriber, RpcPollSubscriber};
pub use verify::{KnotVerifier, SigAlgo, VerificationOutcome, VerificationResult};
pub use witness::{ValidationTestimony, WitnessSubmitter};

/// Canonical wallet address of the ValidationAgent on the cord (per
/// `canonical_ai_agents()` in `rope-explorer`). Forty hex chars.
pub const VALIDATION_AGENT_WALLET: &str = "0x000000000000000000000000000000000000C004";

/// Stable identifier of the canonical agent in `/api/v1/ai-agents`.
pub const VALIDATION_AGENT_ID: &str = "validation";

/// Human-readable name used in testimony metadata.
pub const VALIDATION_AGENT_NAME: &str = "ValidationAgent";

/// Crate semantic version surfaced in testimony metadata.
pub const VALIDATION_AGENT_VERSION: &str = env!("CARGO_PKG_VERSION");
