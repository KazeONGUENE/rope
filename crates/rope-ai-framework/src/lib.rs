//! # Rope AI Agent Framework
//!
//! Pluggable AI agent framework for Datachain Rope. Third-party developers
//! register domain-specific agents that read device/user Strings (ledgers)
//! and write diagnosis or recommendation fragments back.
//!
//! ## Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────┐
//! │              AI Agent Framework               │
//! │                                               │
//! │  ┌───────────┐ ┌───────────┐ ┌───────────┐  │
//! │  │Maintenance│ │  Energy   │ │  Traffic  │  │
//! │  │  Agent    │ │  Agent    │ │  Agent    │  │
//! │  └─────┬─────┘ └─────┬─────┘ └─────┬─────┘  │
//! │        │              │              │        │
//! │  ┌─────┴──────────────┴──────────────┴─────┐ │
//! │  │          Agent Scheduler                 │ │
//! │  │  (lifecycle, scheduling, health checks)  │ │
//! │  └──────────────────┬──────────────────────┘ │
//! │                     │                        │
//! │  ┌──────────────────┴──────────────────────┐ │
//! │  │          String Reader / Writer          │ │
//! │  │  (reads ledger fragments, writes back)   │ │
//! │  └──────────────────────────────────────────┘ │
//! └──────────────────────────────────────────────┘
//! ```
//!
//! ## Built-in Agents (bundled with Rope)
//!
//! | Agent | Domain | Purpose |
//! |-------|--------|---------|
//! | `MaintenanceAgent` | Smart City | Predictive maintenance from IoT telemetry |
//! | `AnomalyAgent` | Universal | Statistical anomaly detection on any String |
//!
//! ## Third-Party Agent Registration
//!
//! Third parties implement the `DomainAgent` trait and register via
//! `rope_registerAgent` RPC or programmatically via `AgentFramework::register()`.

pub mod agent;
pub mod builtin;
pub mod framework;
pub mod scoring;
pub mod types;

pub use agent::DomainAgent;
pub use builtin::{AnomalyAgent, MaintenanceAgent};
pub use framework::{AgentFramework, AgentFrameworkConfig, FrameworkStats};
pub use scoring::{ConfidenceScore, DiagnosisResult, Recommendation, Severity};
pub use types::{AgentCapability, AgentDescriptor, AgentDomain, AgentState, StringQuery};
