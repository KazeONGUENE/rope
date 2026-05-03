//! # rope-deployer — Datachain Foundation cloud provisioning service
//!
//! Lets third parties deploy Datachain Rope nodes onto cloud providers
//! (Exoscale, DigitalOcean) without bringing their own cloud account.
//! Per-tenant isolation is enforced at the IAM, network, and tag layers.
//!
//! See `deploy/EXOSCALE_AS_A_SERVICE.md` for the full architecture.
//!
//! Phase D MVP scope:
//! - HTTP API surface defined and stubbed
//! - `CloudProvider` trait with `local`, `exoscale`, `digitalocean` adapters
//! - Exoscale adapter is dry-run unless `EXOSCALE_API_KEY` is set
//! - DigitalOcean adapter is a stub (Phase E)

pub mod api;
pub mod providers;
pub mod tenant;
pub mod types;

pub use api::AppState;
pub use providers::{CloudProvider, ProviderRegistry};
pub use types::{InstanceInfo, NodeKind, ProvisionRequest, ProvisionResponse, Provider};
