//! # rope-edc - Ecosystem Deployment Console
//!
//! Self-service sovereign infrastructure for predictive maintenance and
//! environmental monitoring on Datachain Rope. Implements the EDC
//! specification v2.0 (`docs/ECOSYSTEM_DEPLOYMENT_CONSOLE_SPEC_V2.md`):
//!
//! * **types** - nine-step wizard data model, node sizing, live facets
//! * **csv_import** - RFC-4180 bulk inventory import
//! * **grants** - AccessGrant engine, API-key minting, Timelock
//! * **registry** - persistent store, JSONL journals, on-chain anchoring
//! * **analytics** - the complete deterministic data-analytics catalogue
//!   (descriptive statistics, time series, anomaly detection, forecasting,
//!   correlation, distribution, clustering, cohort comparison, reliability,
//!   compliance, data quality)
//! * **ai** - AlterOS-orchestrated narration over the deterministic
//!   dossier (Ollama / Anthropic / OpenAI), with a fully-functional
//!   deterministic fallback for sovereign / air-gapped deployments
//! * **api** - Axum router: console API, stakeholder gateway, public
//!   directory for dcscan.io
//! * **simulation** - sandbox mode: deterministic synthetic telemetry +
//!   archetype templates for community testing (v1.0 §6.3)
//! * **walletsig** - EIP-191 wallet-signature stakeholder auth (v1.0 §6.3)
//! * **graphql** - grant-scoped GraphQL endpoint (v1.0 §6.3)
//! * **export** - scheduled bulk CSV extracts per grant (v1.0 §6.3)
//! * **reports** - scheduled statutory/investor reports, anchored (v1.0 §6.4)
//! * **billing** - metering × price-terms billing statements (v1.0 §6.3)
//! * **provision** - NodePlan → rope-deployer cloud provisioning (v1.0 §5)

pub mod ai;
pub mod analytics;
pub mod api;
pub mod billing;
pub mod csv_import;
pub mod export;
pub mod grants;
pub mod graphql;
pub mod nodes;
pub mod provision;
pub mod registry;
pub mod reports;
pub mod session;
pub mod simulation;
pub mod types;
pub mod walletsig;
