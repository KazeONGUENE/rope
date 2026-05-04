//! # InsuranceAgent — Datachain Rope canonical AI testimony agent
//!
//! Issues **parametric-insurance attestations** against tokenized RWAs and
//! anchors them as testimony knots on the chain.
//!
//! Canonical wallet (per `dcscan.io` `/api/v1/ai-agents`): `0x...C003`.
//!
//! ## Pipeline
//!
//! ```text
//!  every --interval-secs:
//!     1. fetch tokenized RWAs from each AssetFeed (Tanastok, NaturaProof, …)
//!     2. for each asset:
//!        - if a recent attestation exists (< --reattest-after-secs): skip
//!        - else: compute RiskProfile → ParametricInsuranceAttestation
//!        - submit to Anchor via rope_appendToLedger (testimony knot)
//!     3. update metrics
//! ```
//!
//! ## Honest scope
//!
//! The risk model is a *parametric formula*: asset-type base premium (bps),
//! per-jurisdiction multiplier, and valuation-based coverage scaling. It is
//! **not** an actuarial model. It produces a deterministic, defensible quote
//! per asset, which is exactly what a parametric attestation is — not "AI
//! underwriting".
//!
//! The NaturaProof feed is a typed stub returning `Vec::new()`. The
//! [`feeds::AssetFeed`] trait is plug-and-play for a real implementation that
//! lives in the NaturaProof project.
//!
//! The anchor is a real JSON-RPC client against `rope_appendToLedger`. The
//! testimony is signed in the OES-managed sense by the node that owns the
//! agent's wallet (i.e., the operator runs this CLI with the agent key).

pub mod anchor;
pub mod attestation;
pub mod config;
pub mod feeds;
pub mod risk;

mod agent;

pub use agent::{AgentMetrics, InsuranceAgent};
pub use anchor::{Anchor, AnchorError, AnchorReceipt, JsonRpcAnchor};
pub use attestation::{
    AttestationDigest, AttestationError, ParametricInsuranceAttestation, TriggerCondition,
};
pub use config::InsuranceAgentConfig;
pub use feeds::{
    naturaproof::NaturaProofStubFeed, tanastok::TanastokFeed, AssetFeed, FeedError, TokenizedAsset,
};
pub use risk::{RiskModel, RiskModelConfig, RiskProfile};

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Canonical InsuranceAgent wallet address on Datachain Rope mainnet
/// (matches `canonical_ai_agents()` in `crates/rope-explorer/src/main.rs`).
pub const CANONICAL_AGENT_WALLET: &str = "0x000000000000000000000000000000000000C003";

/// Canonical agent identifier used in the `agent_id` field of every
/// attestation issued by this crate.
pub const CANONICAL_AGENT_ID: &str = "InsuranceAgent";

/// Default Datachain Rope mainnet RPC endpoint.
pub const DEFAULT_RPC_URL: &str = "https://erpc.datachain.network";

/// Default Tanastok asset API.
pub const DEFAULT_TANASTOK_URL: &str = "https://tanastok.io/api/v1/tokenized-assets?limit=500";
