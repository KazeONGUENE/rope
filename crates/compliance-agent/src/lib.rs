// =============================================================================
// rope-compliance-agent
// =============================================================================
//
// This crate is the canonical home of the Datachain Rope ComplianceAgent
// — one of the five canonical AI testimony agents (id `compliance`,
// wallet `0x000000000000000000000000000000000000C005`) registered in
// `crates/rope-explorer/src/main.rs::canonical_ai_agents()`.
//
// The crate has two surfaces:
//
//   1. The library (everything below) exposes the typed primitives and
//      a `RopeRpcClient` trait that lets integration tests, dApps, or
//      sister agents reuse the same logic without spinning up a real
//      rope-node.
//
//   2. The `compliance-agent` binary (`src/main.rs`) wires those
//      primitives to an axum HTTP listener + a reqwest-backed JSON-RPC
//      client to produce a real running service:
//
//        compliance-agent serve --listen 0.0.0.0:9091 \
//                               --rpc-url http://127.0.0.1:8545
//
// The legacy ERC-3643 transfer validator that previously lived in
// this crate is preserved verbatim under `erc3643_module` so existing
// callers keep working — the new modules sit alongside it.
//
// Author: Kazé A. ONGUENE — Datachain Foundation
// =============================================================================

pub mod erc3643_module;

pub mod anchor;
pub mod config;
pub mod gdpr;
pub mod metrics;
pub mod orchestrator;
pub mod reporting;
pub mod rpc;
pub mod server;
pub mod testimony;

// ---------------------------------------------------------------------------
// Re-exports — the smallest stable surface a downstream caller needs.
// ---------------------------------------------------------------------------

pub use anchor::{AnchorClient, AnchorError, AnchorReceipt};
pub use config::{
    ComplianceAgentConfig, GdprPolicy, CANONICAL_COMPLIANCE_AGENT_WALLET, DEFAULT_LISTEN_ADDR,
    DEFAULT_MAX_DIGEST_EVENTS, DEFAULT_REPORTING_INTERVAL_SECS, DEFAULT_RPC_URL,
};
pub use gdpr::{
    Article17Request, Article17Validator, Article17Verdict, JustificationClass, RejectionReason,
};
pub use metrics::ComplianceMetrics;
pub use orchestrator::{
    OrchestrationError, OrchestrationReport, TombstoneOutcome, UntieOrchestrator,
};
pub use reporting::{PeriodicReporter, ReporterStats, TickOutcome};
pub use rpc::{HttpRopeRpcClient, RopeRpcClient, RpcClientError};
pub use server::{build_router, Article17Response, ServerState};
pub use testimony::{
    ComplianceTestimony, ComplianceTestimonyEnvelope, DoraIncident, DoraIncidentDigest,
    DoraSeverity, DoraSeverityBucket, GdprArticle17Testimony, MiFidIIDigest, MiFidIIEvent,
    MiFidInstrumentBucket,
};
