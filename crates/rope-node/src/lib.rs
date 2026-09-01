//! # Datachain Rope Node
//!
//! Full node implementation for the Datachain Rope network.
//!
//! Architecture: rope-node IS Datachain Rope. It runs consensus natively,
//! produces strings, manages testimony and finality, and coordinates AI agents.
//! An optional EVM execution layer (Reth in production, per
//! `reth-blue-green-ipfs-architecture.mdc`) can be attached as a verifier:
//! when connected, EVM state queries are delegated to it; when absent,
//! rope-node runs fully on its own.
//! Every write transaction is notarized into a RopeString.

pub mod agent_runner;
pub mod config;
pub mod consensus_orchestrator;
pub mod entity_labels;
pub mod entity_manifest;
pub mod evm_backend;
pub mod genesis;
pub mod governance;
pub mod lattice_metrics;
pub mod ledger_manager;
pub mod metrics;
pub mod node;
pub mod oes_key_cache;
pub mod rpc_auth;
pub mod rpc_server;
pub mod rpc_signature;
pub mod self_watchdog;
pub mod string_producer;
pub mod dag_ledger;
pub mod validator_keystore;
/// Per-client-connection bridge from the rope-node WSS listener to
/// Reth's `--ws` port so that `eth_subscribe` / `eth_unsubscribe` and
/// the resulting `eth_subscription` push notifications work end-to-end
/// on `wss://ws.datachain.network` / `wss://ws.rope.network`. See the
/// module docs for the rationale (ChainList red-Score badge closure)
/// and design (per-connection, lazy, verbatim forwarding, no id
/// remapping).
pub mod probe_listener;
pub mod ws_subscription_bridge;

/// Backwards-compatibility re-export for the legacy module path
/// `rope_node::anvil_backend`. The module was renamed `evm_backend`
/// on 2026-05-02 (Anvil was archived 2026-03-31; production runs Reth).
#[deprecated(
    since = "0.2.0",
    note = "Use `rope_node::evm_backend`. The Anvil-era name is preserved \
            only for source compatibility."
)]
pub use evm_backend as anvil_backend;

pub use agent_runner::AgentRunner;
pub use config::NodeConfig;
pub use consensus_orchestrator::{ConsensusOrchestrator, OrchestratorConfig};
pub use evm_backend::{EvmBackend, EvmBackendConfig};
pub use ledger_manager::LedgerManager;
pub use node::RopeNode;
pub use string_producer::{ProductionEvent, ProductionStats, StringProducer, StringProducerConfig};
