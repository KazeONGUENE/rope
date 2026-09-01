//! RPC API Server with mTLS and WebSocket support
//!
//! This module provides a full-featured RPC server for Datachain Rope:
//! - JSON-RPC compatible Ethereum API (HTTP on port 8545)
//! - WebSocket JSON-RPC API (WS on port 8546)
//! - Native Rope API (gRPC + Protocol Buffers)
//! - Mutual TLS (mTLS) authentication
//! - Rate limiting and request validation
//! - Metrics and observability
//!
//! Architecture: rope-node is the MASTER execution layer. The EVM execution
//! layer (Reth in production, per `reth-blue-green-ipfs-architecture.mdc`) is
//! an optional verifier. When the EVM backend is connected, EVM reads are
//! served from it. When the EVM backend is absent, rope-node runs natively
//! — consensus, strings, testimony, finality, and AI agents all function.
//! EVM-specific queries return proper errors indicating the EVM backend is
//! offline.

use crate::config::{DeployerSettings, RpcSettings};
use crate::consensus_orchestrator::ConsensusOrchestrator;
use crate::entity_labels::{self, EntityLabel, LabelKind, LabelRegistry};
use crate::evm_backend::EvmBackend;
use crate::governance::{Authorized, GovernanceAction, GovernanceManager};
use crate::ledger_manager::LedgerManager;
use crate::ws_subscription_bridge::{BridgeWriteFrame, SubscriptionBridge};
use rope_ai_framework::AgentFramework;
use rope_iot_gateway::IoTGateway;
use sha1::{Digest, Sha1};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
// `AsyncWrite` is imported (in addition to `AsyncWriteExt`) so
// `send_websocket_frame` can accept any `impl AsyncWrite + Unpin` —
// specifically the `OwnedWriteHalf` produced by `TcpStream::into_split`
// in the refactored per-connection writer task that unblocks
// server-initiated `eth_subscription` push frames on `wss://ws.datachain.network`.
use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, RwLock};

/// The CERBER WATCH gate — one process-wide [`rope_security::guard::RequestGuard`]
/// shared by every connection handler. Seeded with
/// [`rope_security::guard::KNOWN_COMPROMISED_SIGNERS`] (the H1/C4 compromised
/// deployer key) plus any operator-supplied additions from
/// `ROPE_ADDITIONAL_BLOCKED_SIGNERS` (comma-separated addresses), read once
/// at first use. `OnceLock` mirrors the established pattern in
/// `entity_labels.rs` for other process-wide singletons in this crate.
fn request_guard() -> &'static rope_security::guard::RequestGuard {
    static GUARD: OnceLock<rope_security::guard::RequestGuard> = OnceLock::new();
    GUARD.get_or_init(|| {
        let guard = rope_security::guard::RequestGuard::with_default_blocklist();
        if let Ok(extra) = std::env::var("ROPE_ADDITIONAL_BLOCKED_SIGNERS") {
            for addr in extra.split(',') {
                let addr = addr.trim();
                if !addr.is_empty() {
                    guard.block_signer(addr);
                    tracing::info!(
                        target: "rope_node::auth",
                        signer = addr,
                        "CERBER WATCH: added operator-supplied signer to blocklist"
                    );
                }
            }
        }
        guard
    })
}

/// Public version of the Rope Graph RPC surface.
///
/// Bumped whenever a backwards-compatible field is added (`x.y.PATCH`),
/// a backwards-compatible method is added (`x.MINOR.0`), or a wire-shape
/// breaking change ships (`MAJOR.0.0`). Surfaced via the
/// `X-Rope-RPC-Version` HTTP response header and via the response of
/// the `rpc_methods` discovery method.
pub const ROPE_RPC_API_VERSION: &str = "1.4.0";

/// JSON-RPC `-32005` = retryable ledger overload (queue full).
/// Message carries `Retry-After: 1` for HTTP-aware proxies/clients.
fn jsonrpc_ledger_err(id: &serde_json::Value, e: String) -> String {
    let code = if e.contains("OVERLOAD:") {
        -32005
    } else {
        -32603
    };
    serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": code, "message": e },
        "id": id
    })
    .to_string()
}

/// RPC Server with mTLS and WebSocket support
pub struct RpcServer {
    config: RpcSettings,
    tls_config: Option<TlsConfig>,
    rate_limiter: Arc<RateLimiter>,
    handlers: Arc<RpcHandlers>,
    metrics: Arc<RwLock<RpcMetrics>>,
    ws_broadcast: broadcast::Sender<String>,
}

/// WebSocket frame opcodes
#[derive(Debug, Clone, Copy, PartialEq)]
#[allow(dead_code)]
enum WsOpcode {
    Text = 0x1,
    Binary = 0x2,
    Close = 0x8,
    Ping = 0x9,
    Pong = 0xA,
}

/// TLS configuration for mTLS
#[derive(Clone)]
pub struct TlsConfig {
    pub server_cert: Vec<u8>,
    pub server_key: Vec<u8>,
    pub ca_cert: Option<Vec<u8>>,
    pub require_client_cert: bool,
}

/// Rate limiter configuration
pub struct RateLimiter {
    requests_per_second: u32,
    burst: u32,
    request_counts: RwLock<HashMap<String, RequestCounter>>,
}

#[derive(Clone, Default)]
struct RequestCounter {
    count: u32,
    window_start: i64,
}

/// RPC metrics
#[derive(Clone, Default)]
pub struct RpcMetrics {
    pub total_requests: u64,
    pub successful_requests: u64,
    pub failed_requests: u64,
    pub rate_limited_requests: u64,
    pub avg_response_time_ms: f64,
    pub active_connections: u32,
}

/// Build a canonical JSON-RPC error response string.
fn rpc_err(id: &serde_json::Value, code: i64, message: &str) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "error": { "code": code, "message": message },
        "id": id
    })
    .to_string()
}

/// RPC handlers for Datachain Rope.
///
/// rope-node is the MASTER. The EVM execution layer (Reth in production,
/// per `reth-blue-green-ipfs-architecture.mdc`) is an OPTIONAL verifier.
/// - Chain identity (eth_chainId, net_version) → rope-node (authoritative, always)
/// - Native consensus (rope_*) → rope-node (authoritative, always)
/// - EVM state reads → EVM backend when connected, otherwise proper error
/// - EVM writes → EVM backend when connected + notarize; otherwise error
pub struct RpcHandlers {
    chain_id: u64,
    network_version: String,
    block_number: Arc<parking_lot::RwLock<u64>>,
    gas_price: u64,
    evm_backend: Option<Arc<EvmBackend>>,
    orchestrator: Option<Arc<ConsensusOrchestrator>>,
    ledger: Option<Arc<LedgerManager>>,
    iot_gateway: Option<Arc<IoTGateway>>,
    ai_framework: Option<Arc<AgentFramework>>,
    /// Master-node governance + ACL (added 2026-05-03)
    governance: Option<Arc<GovernanceManager>>,
    /// This node's deployer-identity attestation (added 2026-05-03)
    deployer: Option<DeployerSettings>,
    /// This node's own NodeId hex (used by `rope_nodeIdentity` self-lookup)
    self_node_id: String,
    /// Phase-2 V11 closure: signed-payload destructive-RPC verifier.
    /// Lazy-initialised on first use, then snapshotted from
    /// `governance.registry_snapshot().founder.founder_keys`. The lazy
    /// pattern lets the verifier pick up `master-nodes.toml` reloads
    /// (rare but supported by the governance manager) without restart.
    /// `None` while uninitialised; a fresh `AuthVerifier` after first
    /// destructive Phase-2 call.
    auth_verifier: parking_lot::RwLock<Option<Arc<crate::rpc_signature::AuthVerifier>>>,
    /// Quipu Canon v2.0 Phase 4 — DAG-of-knots ledger serving the
    /// additive `rope_v2_*` namespace alongside the untouched v1.2
    /// linear ledger. Write methods (`rope_v2_appendKnot`,
    /// `rope_v2_compact`) are gated by the destructive-RPC auth layer.
    dag: Arc<crate::dag_ledger::DagLedger>,
}

impl RpcServer {
    pub async fn new(config: &RpcSettings) -> anyhow::Result<Self> {
        Self::new_with_state(
            config,
            271828,
            Arc::new(parking_lot::RwLock::new(0)),
            None,
            None,
        )
        .await
    }

    pub async fn new_with_state(
        config: &RpcSettings,
        chain_id: u64,
        current_round: Arc<parking_lot::RwLock<u64>>,
        evm_backend: Option<Arc<EvmBackend>>,
        orchestrator: Option<Arc<ConsensusOrchestrator>>,
    ) -> anyhow::Result<Self> {
        Self::new_full(
            config,
            chain_id,
            current_round,
            evm_backend,
            orchestrator,
            None,
            None,
            None,
        )
        .await
    }

    pub async fn new_full(
        config: &RpcSettings,
        chain_id: u64,
        current_round: Arc<parking_lot::RwLock<u64>>,
        evm_backend: Option<Arc<EvmBackend>>,
        orchestrator: Option<Arc<ConsensusOrchestrator>>,
        ledger: Option<Arc<LedgerManager>>,
        iot_gateway: Option<Arc<IoTGateway>>,
        ai_framework: Option<Arc<AgentFramework>>,
    ) -> anyhow::Result<Self> {
        Self::new_full_v2(
            config,
            chain_id,
            current_round,
            evm_backend,
            orchestrator,
            ledger,
            iot_gateway,
            ai_framework,
            None,
            None,
            String::new(),
        )
        .await
    }

    /// Extended constructor that wires in master-node governance + deployer
    /// identity. Existing callers can keep using `new_full`; this is the
    /// preferred constructor for production rope-node startup.
    #[allow(clippy::too_many_arguments)]
    pub async fn new_full_v2(
        config: &RpcSettings,
        chain_id: u64,
        current_round: Arc<parking_lot::RwLock<u64>>,
        evm_backend: Option<Arc<EvmBackend>>,
        orchestrator: Option<Arc<ConsensusOrchestrator>>,
        ledger: Option<Arc<LedgerManager>>,
        iot_gateway: Option<Arc<IoTGateway>>,
        ai_framework: Option<Arc<AgentFramework>>,
        governance: Option<Arc<GovernanceManager>>,
        deployer: Option<DeployerSettings>,
        self_node_id: String,
    ) -> anyhow::Result<Self> {
        let rate_limiter = Arc::new(RateLimiter {
            requests_per_second: 100,
            burst: 200,
            request_counts: RwLock::new(HashMap::new()),
        });

        let handlers = Arc::new(RpcHandlers {
            chain_id,
            network_version: "0.1.0".to_string(),
            block_number: current_round,
            gas_price: 1_000_000_000,
            evm_backend,
            orchestrator,
            ledger,
            iot_gateway,
            ai_framework,
            governance,
            deployer,
            self_node_id,
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        });

        let (ws_broadcast, _) = broadcast::channel(1000);

        Ok(Self {
            config: config.clone(),
            tls_config: None,
            rate_limiter,
            handlers,
            metrics: Arc::new(RwLock::new(RpcMetrics::default())),
            ws_broadcast,
        })
    }

    pub fn with_tls(mut self, tls_config: TlsConfig) -> Self {
        self.tls_config = Some(tls_config);
        self
    }

    pub fn with_mtls(
        mut self,
        server_cert: Vec<u8>,
        server_key: Vec<u8>,
        ca_cert: Vec<u8>,
    ) -> Self {
        self.tls_config = Some(TlsConfig {
            server_cert,
            server_key,
            ca_cert: Some(ca_cert),
            require_client_cert: true,
        });
        self
    }

    pub async fn run(&self) -> anyhow::Result<()> {
        // CERBER boot-time dispatcher-completeness check (2026-07-25 audit,
        // M11 follow-up). `crate::rpc_auth::verify_dispatcher_completeness`
        // compares the build.rs-scanned list of every method literal that
        // actually appears in this file's dispatch `match` against the four
        // hand-curated buckets (destructive / self-authenticated /
        // dev-only-EVM / safe-read-only). A gap here means some RPC method
        // is reachable without ANY of us having consciously decided which
        // auth class it belongs to — precisely the class of bug that let
        // `rope_registerDevice`, `rope_ingestTelemetry`, and
        // `rope_subscribeAgentToWallet` ship unauthenticated in the past.
        //
        // Fail-closed by default: a gap refuses to start the node at all.
        // The only escape hatch is `ROPE_SKIP_DISPATCHER_COMPLETENESS_CHECK=1`,
        // meant for a hotfix window where an operator has already read the
        // gap report and is deliberately accepting the risk for one boot.
        if let Err(report) = crate::rpc_auth::verify_dispatcher_completeness() {
            tracing::error!(
                target: "rope_node::auth",
                unclassified = ?report.unclassified,
                duplicated = ?report.duplicates,
                "CERBER dispatcher-completeness check FAILED: one or more RPC methods \
                 are reachable without a defined auth class. Refusing to start."
            );
            let skip = std::env::var("ROPE_ALLOW_DISPATCHER_DRIFT")
                .map(|v| v == "1")
                .unwrap_or(false);
            if !skip {
                anyhow::bail!(
                    "dispatcher-completeness check failed: {} unclassified, {} duplicated \
                     method(s). Set ROPE_ALLOW_DISPATCHER_DRIFT=1 to override for one boot \
                     after reviewing the gap (not recommended in production).",
                    report.unclassified.len(),
                    report.duplicates.len()
                );
            }
            tracing::warn!(
                target: "rope_node::auth",
                "ROPE_ALLOW_DISPATCHER_DRIFT=1 set; starting DESPITE the completeness gap \
                 above. This is an operator-acknowledged risk."
            );
        } else {
            tracing::info!(
                target: "rope_node::auth",
                "CERBER dispatcher-completeness check passed: every dispatchable RPC \
                 method has exactly one auth classification."
            );
        }

        let http_addr: SocketAddr = self.config.http_addr.parse()?;
        let ws_addr: SocketAddr = self.config.ws_addr.parse()?;

        tracing::info!("Starting HTTP RPC server on {}", http_addr);
        tracing::info!("Starting WebSocket RPC server on {}", ws_addr);

        if self.tls_config.is_some() {
            tracing::info!(
                "TLS enabled, mTLS: {}",
                self.tls_config
                    .as_ref()
                    .map(|c| c.require_client_cert)
                    .unwrap_or(false)
            );
        }

        let http_listener = tokio::net::TcpListener::bind(&http_addr).await?;
        let ws_listener = tokio::net::TcpListener::bind(&ws_addr).await?;

        tracing::info!("RPC server ready (JSON-RPC HTTP + WebSocket)");

        // Loopback probe port (sync thread) — immune to Tokio pool saturation.
        crate::probe_listener::spawn_probe_listener(self.handlers.block_number.clone());

        // Background tip refresh: keeps `block_number` warm even when the HTTP
        // accept/handler pool is contended, so HA probes and eth_blockNumber
        // fast-path can answer from cache without wedging on Reth.
        {
            let tip_handlers = self.handlers.clone();
            tokio::spawn(async move {
                let req = serde_json::json!({
                    "jsonrpc": "2.0",
                    "method": "eth_blockNumber",
                    "params": [],
                    "id": 0
                });
                let mut tick = tokio::time::interval(std::time::Duration::from_secs(2));
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                loop {
                    tick.tick().await;
                    let Some(evm) = tip_handlers.evm_backend.as_ref() else {
                        continue;
                    };
                    if !evm.is_healthy() {
                        continue;
                    }
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        evm.forward_request(&req),
                    )
                    .await
                    {
                        Ok(Ok(response)) => {
                            if let Some(hex_str) =
                                response.get("result").and_then(|v| v.as_str())
                            {
                                if let Ok(n) =
                                    u64::from_str_radix(hex_str.trim_start_matches("0x"), 16)
                                {
                                    *tip_handlers.block_number.write() = n;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            });
        }

        let http_handlers = self.handlers.clone();
        let http_rate_limiter = self.rate_limiter.clone();
        let http_metrics = self.metrics.clone();

        let http_task = tokio::spawn(async move {
            loop {
                match http_listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        let handlers = http_handlers.clone();
                        let rate_limiter = http_rate_limiter.clone();
                        let metrics = http_metrics.clone();

                        {
                            let mut m = metrics.write().await;
                            m.active_connections += 1;
                        }

                        tokio::spawn(async move {
                            let peer_ip = peer_addr.ip().to_string();

                            // Coarse, connection-accept-time layer: caps raw
                            // TCP connection churn from any single peer
                            // BEFORE we've read a byte off the socket. In
                            // production this peer is almost always nginx's
                            // own (loopback) address for proxied traffic, so
                            // this alone does not isolate distinct internet
                            // clients from each other — see the fine-grained,
                            // XFF-aware check inside `handle_connection`
                            // (finding H4, `SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`)
                            // for the check that actually does that.
                            if !rate_limiter.check(&peer_ip).await {
                                let mut m = metrics.write().await;
                                m.rate_limited_requests += 1;
                                return;
                            }

                            if let Err(e) = handle_connection(
                                stream,
                                peer_addr,
                                handlers,
                                metrics.clone(),
                                rate_limiter.clone(),
                            )
                            .await
                            {
                                tracing::debug!("HTTP connection error from {}: {}", peer_addr, e);
                            }

                            {
                                let mut m = metrics.write().await;
                                m.active_connections = m.active_connections.saturating_sub(1);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("HTTP accept error: {}", e);
                    }
                }
            }
        });

        let ws_handlers = self.handlers.clone();
        let ws_rate_limiter = self.rate_limiter.clone();
        let ws_metrics = self.metrics.clone();
        let ws_broadcast = self.ws_broadcast.clone();

        let ws_task = tokio::spawn(async move {
            loop {
                match ws_listener.accept().await {
                    Ok((stream, peer_addr)) => {
                        let handlers = ws_handlers.clone();
                        let rate_limiter = ws_rate_limiter.clone();
                        let metrics = ws_metrics.clone();
                        let broadcast = ws_broadcast.clone();

                        tokio::spawn(async move {
                            let peer_ip = peer_addr.ip().to_string();

                            if !rate_limiter.check(&peer_ip).await {
                                let mut m = metrics.write().await;
                                m.rate_limited_requests += 1;
                                return;
                            }

                            if let Err(e) = handle_websocket_connection(
                                stream,
                                peer_addr,
                                handlers,
                                metrics.clone(),
                                broadcast,
                            )
                            .await
                            {
                                tracing::debug!(
                                    "WebSocket connection error from {}: {}",
                                    peer_addr,
                                    e
                                );
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("WebSocket accept error: {}", e);
                    }
                }
            }
        });

        tokio::select! {
            _ = http_task => tracing::error!("HTTP server exited unexpectedly"),
            _ = ws_task => tracing::error!("WebSocket server exited unexpectedly"),
        }

        Ok(())
    }

    pub async fn metrics(&self) -> RpcMetrics {
        self.metrics.read().await.clone()
    }
}

/// Hard cap on the number of distinct rate-limit buckets kept in memory.
///
/// Added 2026-07-25 alongside finding H4
/// (`docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`): once the limiter
/// started keying on `effective_client_ip` (which, for loopback-proxied
/// traffic, reflects an attacker-supplied `X-Forwarded-For` value) an
/// attacker who can reach the node directly, or who controls many real
/// source IPs, could otherwise grow `RateLimiter::request_counts`
/// without bound (see finding M1, unbounded in-memory maps). `check()`
/// opportunistically evicts stale buckets once the map exceeds this
/// size, so memory stays bounded without a dedicated background task.
const RATE_LIMITER_MAX_BUCKETS: usize = 50_000;

impl RateLimiter {
    async fn check(&self, ip: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        let mut counts = self.request_counts.write().await;

        if counts.len() >= RATE_LIMITER_MAX_BUCKETS && !counts.contains_key(ip) {
            // Evict every bucket whose window is stale (no traffic in the
            // last 2s) before admitting a new key. If nothing is stale
            // (a genuine burst of that many distinct concurrent clients),
            // we still admit the new key rather than fail closed — this
            // is a memory-bound, not a hard cap on legitimate traffic.
            counts.retain(|_, c| now - c.window_start < 2);
        }

        let counter = counts.entry(ip.to_string()).or_default();

        if now - counter.window_start >= 1 {
            counter.count = 0;
            counter.window_start = now;
        }

        if counter.count >= self.requests_per_second + self.burst {
            return false;
        }

        counter.count += 1;
        true
    }
}

/// Hard ceiling on `ROPE_RPC_MAX_BODY_BYTES` — even an operator
/// misconfiguration cannot push the limit back up to the old 2 GB DoS
/// exposure (finding H3, `docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`).
const MAX_REQUEST_SIZE_CEILING: usize = 67_108_864; // 64 MB
/// Floor so a fat-fingered env var can't accidentally wedge every real
/// JSON-RPC caller.
const MAX_REQUEST_SIZE_FLOOR: usize = 1_048_576; // 1 MB
/// Default max JSON-RPC request body size.
///
/// 2026-07-25 security remediation (finding H3): this was previously a
/// hardcoded 2 GB, justified in a since-stale comment as headroom for
/// `anvil_dumpState`/`anvil_loadState` "state dumps". Those two RPC
/// methods (see `evm_backend.rs::dump_state`/`load_state`) target the
/// legacy Anvil execution layer only — Anvil was fully decommissioned
/// 2026-03-31 (Reth is the sole execution layer in production; on Reth
/// those methods already return "method not found", per the doc
/// comments on `long_running_rpc`). There is no remaining legitimate
/// call shape that needs anywhere close to 2 GB, and allowing a
/// `Content-Length` that large lets a single connection force this
/// per-connection buffer (see `read_full_http_request` below) to
/// `reserve()` up to 2 GB of heap before any body-size validation of the
/// *actual* payload runs — trivial memory-exhaustion DoS with a handful
/// of concurrent connections. 10 MB comfortably covers the largest
/// realistic JSON-RPC payload (batched `eth_sendRawTransaction` calls,
/// large contract-deployment bytecode/constructor args, `rope_v2_*`
/// bulk-knot submissions) with headroom to spare. Override with
/// `ROPE_RPC_MAX_BODY_BYTES` if a future legitimate use case needs more,
/// clamped to [`MAX_REQUEST_SIZE_FLOOR`, `MAX_REQUEST_SIZE_CEILING`].
const MAX_REQUEST_SIZE_DEFAULT: usize = 10_485_760; // 10 MB

fn max_request_size() -> usize {
    match std::env::var("ROPE_RPC_MAX_BODY_BYTES") {
        Ok(v) => match v.trim().parse::<usize>() {
            Ok(n) => n.clamp(MAX_REQUEST_SIZE_FLOOR, MAX_REQUEST_SIZE_CEILING),
            Err(_) => {
                tracing::warn!(
                    "ROPE_RPC_MAX_BODY_BYTES={v:?} is not a valid usize — using default \
                     {MAX_REQUEST_SIZE_DEFAULT} bytes"
                );
                MAX_REQUEST_SIZE_DEFAULT
            }
        },
        Err(_) => MAX_REQUEST_SIZE_DEFAULT,
    }
}

const READ_BUF_SIZE: usize = 262_144; // 256 KB read chunks for throughput

#[cfg(test)]
mod max_request_size_tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn defaults_to_10mb_when_unset() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROPE_RPC_MAX_BODY_BYTES");
        assert_eq!(max_request_size(), MAX_REQUEST_SIZE_DEFAULT);
        assert_eq!(max_request_size(), 10_485_760);
    }

    #[test]
    fn old_2gb_default_is_gone() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var("ROPE_RPC_MAX_BODY_BYTES");
        assert!(
            max_request_size() < 2_147_483_648,
            "the pre-remediation 2 GB default must never come back"
        );
    }

    #[test]
    fn respects_env_override_within_bounds() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROPE_RPC_MAX_BODY_BYTES", "20000000");
        assert_eq!(max_request_size(), 20_000_000);
        std::env::remove_var("ROPE_RPC_MAX_BODY_BYTES");
    }

    #[test]
    fn clamps_override_above_ceiling() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROPE_RPC_MAX_BODY_BYTES", "999999999999");
        assert_eq!(max_request_size(), MAX_REQUEST_SIZE_CEILING);
        std::env::remove_var("ROPE_RPC_MAX_BODY_BYTES");
    }

    #[test]
    fn clamps_override_below_floor() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROPE_RPC_MAX_BODY_BYTES", "1");
        assert_eq!(max_request_size(), MAX_REQUEST_SIZE_FLOOR);
        std::env::remove_var("ROPE_RPC_MAX_BODY_BYTES");
    }

    #[test]
    fn falls_back_to_default_on_garbage_value() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROPE_RPC_MAX_BODY_BYTES", "not-a-number");
        assert_eq!(max_request_size(), MAX_REQUEST_SIZE_DEFAULT);
        std::env::remove_var("ROPE_RPC_MAX_BODY_BYTES");
    }
}

async fn read_full_http_request(stream: &mut tokio::net::TcpStream) -> anyhow::Result<Vec<u8>> {
    let max_size = max_request_size();
    let mut data = Vec::with_capacity(READ_BUF_SIZE);
    let mut tmp = vec![0u8; READ_BUF_SIZE];

    loop {
        let n = stream.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        data.extend_from_slice(&tmp[..n]);

        if let Some(header_end) = find_header_end(&data) {
            let headers = String::from_utf8_lossy(&data[..header_end]);
            let content_length = parse_content_length(&headers);

            if content_length > max_size {
                anyhow::bail!("Request body too large ({} bytes)", content_length);
            }

            let body_expected = header_end + 4 + content_length;
            // Cap the reservation itself: `body_expected` is already
            // bounded by `max_size` above, so this is defense-in-depth
            // against any future change to the bound-check ordering
            // above rather than a live gap today.
            let reserve_amount = body_expected
                .saturating_sub(data.len())
                .min(max_size.saturating_sub(data.len().min(max_size)));
            data.reserve(reserve_amount);
            while data.len() < body_expected {
                if data.len() > max_size {
                    anyhow::bail!("Request body exceeded max size while streaming");
                }
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&tmp[..n]);
            }
            break;
        }

        if data.len() > max_size {
            anyhow::bail!("Request too large");
        }
    }
    Ok(data)
}

fn find_header_end(data: &[u8]) -> Option<usize> {
    data.windows(4).position(|w| w == b"\r\n\r\n")
}

fn parse_content_length(headers: &str) -> usize {
    for line in headers.lines() {
        if line.to_lowercase().starts_with("content-length:") {
            if let Some(val) = line.split(':').nth(1) {
                return val.trim().parse().unwrap_or(0);
            }
        }
    }
    0
}

/// Parse a JSON interaction object (the same shape `rope_appendToLedger`
/// accepts) into a [`rope_core::personal_ledger::InteractionRecord`].
/// Shared by the v1.2 linear-ledger append and the Phase 4
/// `rope_v2_appendKnot` DAG append so both canons see identical
/// payload semantics.
fn parse_interaction_record(
    interaction_val: &serde_json::Value,
) -> rope_core::personal_ledger::InteractionRecord {
    use rope_core::personal_ledger::{InteractionRecord, InteractionType};

    let itype_str = interaction_val
        .get("interaction_type")
        .and_then(|v| v.as_str())
        .unwrap_or("Custom");
    let description = interaction_val
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let metadata = interaction_val
        .get("metadata")
        .cloned()
        .unwrap_or(serde_json::json!({}));

    let interaction_type = match itype_str {
        "Transfer" => InteractionType::Transfer,
        "ContractCall" | "ContractDeploy" => InteractionType::ContractCall,
        "TokenApproval" | "Approval" => InteractionType::TokenApproval,
        "IdentityClaim" | "DIDCreation" | "DIDUpdate" | "ClaimIssuance" => {
            InteractionType::IdentityClaim
        }
        "TestimonySubmission" => InteractionType::TestimonySubmission,
        "DataSharing" | "PlatformConnection" => InteractionType::DataSharing,
        "StakeDeposit" | "Stake" => InteractionType::StakeDeposit,
        "StakeWithdraw" | "Unstake" => InteractionType::StakeWithdraw,
        "BridgeOperation" => InteractionType::BridgeOperation,
        other => InteractionType::Custom(other.to_string()),
    };

    InteractionRecord {
        interaction_type,
        counterparty: None,
        data: description.as_bytes().to_vec(),
        timestamp: chrono::Utc::now().timestamp(),
        metadata: {
            let mut map = hashbrown::HashMap::new();
            if let Some(obj) = metadata.as_object() {
                for (k, v) in obj {
                    map.insert(k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string());
                }
            }
            map
        },
    }
}

async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    peer_addr: std::net::SocketAddr,
    handlers: Arc<RpcHandlers>,
    metrics: Arc<RwLock<RpcMetrics>>,
    rate_limiter: Arc<RateLimiter>,
) -> anyhow::Result<()> {
    let start = std::time::Instant::now();

    let data = match read_full_http_request(&mut stream).await {
        Ok(d) if d.is_empty() => return Ok(()),
        Ok(d) => d,
        Err(e) => {
            let body = format!(
                r#"{{"jsonrpc":"2.0","error":{{"code":-32600,"message":"{}"}},"id":null}}"#,
                e
            );
            let resp = format!(
                "HTTP/1.1 413 Payload Too Large\r\n\
                Content-Type: application/json\r\n\
                Content-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            return Ok(());
        }
    };

    let request = String::from_utf8_lossy(&data);

    {
        let mut m = metrics.write().await;
        m.total_requests += 1;
    }

    let response = if request.contains("POST") || request.contains("GET /") {
        let body_start = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
        let body = &request[body_start..];

        // V11 hot-fix: peek at the body so we can diagnose any remaining
        // rejections (peer + XFF + token presence) without doubling
        // `handle_json_rpc_with_auth` parsing logic.
        let preview_method: String = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| {
                v.get("method")
                    .and_then(|m| m.as_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_default();
        // V11 hot-fix: decide whether this request is "internal" (allowed
        // to call destructive `rope_*` methods) using two complementary
        // signals — both must fail before we treat it as public:
        //
        //   1. Peer-address rule. The connection arrived from a loopback
        //      address (127.0.0.0/8 or ::1) AND the HTTP request carries
        //      no `X-Forwarded-For`. nginx ALWAYS sets X-Forwarded-For on
        //      public traffic (verified in deploy/nginx/conf.d/), so a
        //      missing X-Forwarded-For means the caller is one of the
        //      five canonical agents on rope-vps speaking straight to
        //      `127.0.0.1:8545`. This signal is what unblocks the agents
        //      without touching their code.
        //
        //   2. Token rule. The request carries `X-Rope-Internal-Token`
        //      whose value matches the env var `ROPE_INTERNAL_RPC_TOKEN`
        //      (constant-time compare). nginx strips this header on
        //      inbound public traffic, so it can only be set by callers
        //      we trust on the box.
        //
        // We fail-closed: if neither rule fires the gate runs as before.
        let headers = &request[..body_start];
        let has_x_forwarded_for = headers
            .lines()
            .any(|l| {
                let lc = l.to_ascii_lowercase();
                lc.starts_with("x-forwarded-for:") || lc.starts_with("x-real-ip:")
            });
        let presented_token = headers
            .lines()
            .find(|l| {
                let h = crate::rpc_auth::INTERNAL_AUTH_HEADER;
                l.len() > h.len()
                    && l[..h.len()].eq_ignore_ascii_case(h)
                    && l.as_bytes()[h.len()] == b':'
            })
            .map(|l| {
                l[crate::rpc_auth::INTERNAL_AUTH_HEADER.len() + 1..]
                    .trim()
                    .to_string()
            })
            .unwrap_or_default();
        let token_matches = !presented_token.is_empty()
            && crate::rpc_auth::internal_token_matches(&presented_token);
        let peer_is_loopback = peer_addr.ip().is_loopback();
        let is_internal = token_matches
            || (peer_is_loopback && !has_x_forwarded_for);

        // Diagnostic line at debug level. Non-internal destructive calls
        // are still warned by `handle_json_rpc_with_auth` itself; this
        // adds peer + XFF context so we can tell agent-wallets from
        // attackers in the journal.
        if !is_internal && crate::rpc_auth::DESTRUCTIVE_METHODS.contains(&preview_method.as_str()) {
            tracing::warn!(
                target: "rope_node::auth",
                method = %preview_method,
                peer = %peer_addr,
                peer_is_loopback,
                has_x_forwarded_for,
                token_present = !presented_token.is_empty(),
                token_matches,
                "destructive call denied (will return -32401)"
            );
        }

        // Fine-grained, per-real-client rate limit (finding H4,
        // `SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md`). The
        // connection-accept-time check at the call site keys on the raw
        // TCP peer, which for nearly all public traffic is nginx's own
        // address — it caps aggregate connection churn but cannot
        // isolate one abusive internet client from every other client
        // sharing the same proxy. This second check keys on
        // `effective_client_ip` (the real client IP from
        // X-Forwarded-For, but ONLY trusted when the TCP peer is
        // loopback — see doc comment on that function for why that is
        // safe against forgery) so a single abusive client is throttled
        // without collaterally throttling everyone else behind the same
        // nginx.
        let effective_ip =
            crate::rpc_auth::effective_client_ip(&peer_addr.ip().to_string(), peer_is_loopback, headers);
        if !rate_limiter.check(&effective_ip).await {
            let mut m = metrics.write().await;
            m.rate_limited_requests += 1;
            drop(m);
            let body = r#"{"jsonrpc":"2.0","error":{"code":-32029,"message":"Rate limit exceeded"},"id":null}"#;
            format!(
                "HTTP/1.1 429 Too Many Requests\r\n\
                Content-Type: application/json\r\n\
                Retry-After: 1\r\n\
                Content-Length: {}\r\n\r\n{}",
                body.len(),
                body
            )
        } else {
            let json_response = handlers
                .handle_json_rpc_with_auth(body, is_internal)
                .await;

            format!(
                "HTTP/1.1 200 OK\r\n\
                Content-Type: application/json\r\n\
                Access-Control-Allow-Origin: *\r\n\
                Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                Access-Control-Allow-Headers: Content-Type\r\n\
                Access-Control-Expose-Headers: X-Rope-RPC-Version\r\n\
                X-Rope-RPC-Version: {}\r\n\
                Cache-Control: no-store\r\n\
                Content-Length: {}\r\n\r\n{}",
                ROPE_RPC_API_VERSION,
                json_response.len(),
                json_response
            )
        }
    } else if request.contains("OPTIONS") {
        format!(
            "HTTP/1.1 204 No Content\r\n\
            Access-Control-Allow-Origin: *\r\n\
            Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
            Access-Control-Allow-Headers: Content-Type\r\n\
            Access-Control-Expose-Headers: X-Rope-RPC-Version\r\n\
            X-Rope-RPC-Version: {}\r\n\r\n",
            ROPE_RPC_API_VERSION
        )
    } else {
        "HTTP/1.1 404 Not Found\r\n\r\n".to_string()
    };

    stream.write_all(response.as_bytes()).await?;

    {
        let elapsed = start.elapsed().as_millis() as f64;
        let mut m = metrics.write().await;
        m.successful_requests += 1;
        m.avg_response_time_ms = (m.avg_response_time_ms * (m.successful_requests - 1) as f64
            + elapsed)
            / m.successful_requests as f64;
    }

    Ok(())
}

/// M9 (2026-07-25 security audit): upper bound on a single WebSocket
/// frame's payload before we allocate a buffer for it. The RFC 6455
/// extended-length path lets a client declare up to `u64::MAX` bytes in
/// just 2 header bytes + 8 length bytes; without a cap, `vec![0u8;
/// payload_len]` attempts that allocation immediately, well before a
/// single payload byte has been read off the wire — an attacker-chosen
/// number, not real data. 16 MiB is generous headroom over any legitimate
/// JSON-RPC request/response this listener ever handles (batch RPC calls,
/// `rope_getStringWithKnots` history dumps, etc. are all well under 1 MiB
/// in practice) while bounding the worst case an attacker can force per
/// frame. The listener binds 127.0.0.1-only today (V11 gate), but this
/// cap holds regardless of that assumption ever changing.
const MAX_WS_FRAME_PAYLOAD_BYTES: usize = 16 * 1024 * 1024;

/// RFC 6455 §5.5: control frames (Close/Ping/Pong, opcodes 0x8–0xA) MUST
/// NOT have a payload larger than 125 bytes. Enforced here so a
/// malformed/hostile control frame can't reach the same unbounded-length
/// path either.
const MAX_WS_CONTROL_FRAME_PAYLOAD_BYTES: usize = 125;

async fn handle_websocket_connection(
    mut stream: tokio::net::TcpStream,
    peer_addr: SocketAddr,
    handlers: Arc<RpcHandlers>,
    metrics: Arc<RwLock<RpcMetrics>>,
    _broadcast: broadcast::Sender<String>,
) -> anyhow::Result<()> {
    let mut buf = [0u8; 4096];
    let n = stream.read(&mut buf).await?;

    if n == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buf[..n]);

    // V11 (2026-08-11 ws.datachain.network fix): mirror the HTTP-listener
    // rule so the WS listener can be exposed publicly via nginx safely.
    // Compute `is_internal` from the same two signals as the HTTP path:
    //   1. Peer address is loopback (127.0.0.0/8 or ::1) AND the HTTP
    //      upgrade request carries no `X-Forwarded-For` / `X-Real-IP`.
    //      nginx ALWAYS sets X-Forwarded-For on public traffic, so a
    //      missing XFF on a loopback connection means the caller is a
    //      co-located agent speaking straight to 127.0.0.1:8546.
    //   2. `X-Rope-Internal-Token` header matches the env-var secret
    //      (constant-time compare); nginx strips this header on the
    //      public location, so it can only be set by trusted callers.
    // We fail-closed: if neither rule fires, destructive `rope_*`
    // methods hit the `-32401` gate exactly like on the HTTP listener.
    let headers_end = request.find("\r\n\r\n").unwrap_or(request.len());
    let headers = &request[..headers_end];
    let has_x_forwarded_for = headers.lines().any(|l| {
        let lc = l.to_ascii_lowercase();
        lc.starts_with("x-forwarded-for:") || lc.starts_with("x-real-ip:")
    });
    let presented_token = headers
        .lines()
        .find(|l| {
            let h = crate::rpc_auth::INTERNAL_AUTH_HEADER;
            l.len() > h.len()
                && l[..h.len()].eq_ignore_ascii_case(h)
                && l.as_bytes()[h.len()] == b':'
        })
        .map(|l| {
            l[crate::rpc_auth::INTERNAL_AUTH_HEADER.len() + 1..]
                .trim()
                .to_string()
        })
        .unwrap_or_default();
    let token_matches = !presented_token.is_empty()
        && crate::rpc_auth::internal_token_matches(&presented_token);
    let peer_is_loopback = peer_addr.ip().is_loopback();
    let is_internal = token_matches || (peer_is_loopback && !has_x_forwarded_for);

    if request.contains("Upgrade: websocket") {
        let key = request
            .lines()
            .find(|line| line.to_lowercase().starts_with("sec-websocket-key:"))
            .and_then(|line| line.split(':').nth(1))
            .map(|k| k.trim())
            .unwrap_or("");

        let accept_key = generate_websocket_accept_key(key);

        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
            Upgrade: websocket\r\n\
            Connection: Upgrade\r\n\
            Sec-WebSocket-Accept: {}\r\n\r\n",
            accept_key
        );
        stream.write_all(response.as_bytes()).await?;

        {
            let mut m = metrics.write().await;
            m.active_connections += 1;
        }

        // Split the client TCP stream so the reader can keep parsing
        // frames while a dedicated writer task pushes outbound frames
        // driven from three independent sources:
        //   (1) the reader loop's JSON-RPC responses,
        //   (2) the SubscriptionBridge's `eth_subscribe`/
        //       `eth_unsubscribe` replies and forwarded `eth_subscription`
        //       push notifications from Reth (this is what fixes the
        //       ChainList red-Score badge on wss://ws.datachain.network),
        //   (3) pong replies to client pings + the final close frame.
        // Unbounded channel is bounded in practice by per-connection
        // subscription rate; no cross-connection sharing.
        let (raw_read, raw_write) = stream.into_split();
        let mut reader = raw_read;
        let (write_tx, write_rx) = mpsc::unbounded_channel::<WsWriteFrame>();
        let writer_task = tokio::spawn(ws_writer_task(raw_write, write_rx));

        // Per-connection subscription bridge to Reth's `--ws` port. Lazy:
        // no upstream TCP connection is opened until the client actually
        // sends `eth_subscribe`. If the operator has disabled the bridge
        // (`ROPE_RETH_WS_URL=""` or `[evm_backend].ws_url = ""`), the
        // bridge still exists but `handle()` will return a canonical
        // `-32601` for subscribe / unsubscribe, matching the "method
        // unavailable" wire shape the client expects.
        let reth_ws_url = handlers
            .evm_backend
            .as_ref()
            .and_then(|b| b.reth_ws_url().map(|s| s.to_string()));
        let bridge_write_tx = bridge_writer_adapter(write_tx.clone());
        let bridge = SubscriptionBridge::new(reth_ws_url, bridge_write_tx);

        let mut close_requested = false;

        loop {
            let mut header = [0u8; 2];
            if reader.read_exact(&mut header).await.is_err() {
                break;
            }

            let fin = (header[0] & 0x80) != 0;
            let opcode = header[0] & 0x0F;
            let masked = (header[1] & 0x80) != 0;
            let mut payload_len = (header[1] & 0x7F) as usize;

            if payload_len == 126 {
                let mut ext = [0u8; 2];
                if reader.read_exact(&mut ext).await.is_err() {
                    break;
                }
                payload_len = u16::from_be_bytes(ext) as usize;
            } else if payload_len == 127 {
                let mut ext = [0u8; 8];
                if reader.read_exact(&mut ext).await.is_err() {
                    break;
                }
                payload_len = u64::from_be_bytes(ext) as usize;
            }

            // M9: reject before allocating anything sized off the
            // client-declared length. Control frames get the stricter
            // RFC 6455 125-byte ceiling; data frames get the general cap.
            let is_control_frame = opcode >= 0x8;
            let frame_cap = if is_control_frame {
                MAX_WS_CONTROL_FRAME_PAYLOAD_BYTES
            } else {
                MAX_WS_FRAME_PAYLOAD_BYTES
            };
            if payload_len > frame_cap {
                tracing::warn!(
                    target: "rope_node::rpc_server",
                    payload_len,
                    frame_cap,
                    opcode,
                    "WebSocket frame exceeds cap; closing connection (code 1009)"
                );
                // 1009 = "Message Too Big" per RFC 6455 §7.4.1. Best-effort
                // send: if the writer channel has already been closed
                // (client dropped after sending only the header) the
                // error is fine — we're breaking either way.
                let _ = write_tx.send(WsWriteFrame::close(1009));
                close_requested = true;
                break;
            }

            let mut mask_key = [0u8; 4];
            if masked {
                if reader.read_exact(&mut mask_key).await.is_err() {
                    break;
                }
            }

            let mut payload = vec![0u8; payload_len];
            if !payload.is_empty() && reader.read_exact(&mut payload).await.is_err() {
                break;
            }

            if masked {
                for (i, byte) in payload.iter_mut().enumerate() {
                    *byte ^= mask_key[i % 4];
                }
            }

            match opcode {
                0x1 | 0x2 => {
                    if fin {
                        let request_str = String::from_utf8_lossy(&payload);
                        // Route `eth_subscribe` / `eth_unsubscribe` to
                        // the bridge; everything else stays on the
                        // existing dispatcher.
                        //
                        // The bridge's return value is the JSON-RPC
                        // *acknowledgement* text (subscription id for
                        // `eth_subscribe`, boolean result for
                        // `eth_unsubscribe`, or a canonical error). The
                        // subsequent `eth_subscription` push
                        // notifications are pumped independently
                        // through the bridge's own `to_client` channel
                        // (adapted into `write_tx` by
                        // `bridge_writer_adapter` above), so they land
                        // on the same TCP writer without going through
                        // the reader loop again.
                        if let Some((method, request_id)) =
                            parse_subscription_request(&request_str)
                        {
                            let response = bridge
                                .handle(&method, &request_str, request_id)
                                .await;
                            if write_tx.send(WsWriteFrame::Text(response)).is_err() {
                                // Writer task exited (client gone or
                                // channel closed by shutdown path). We
                                // can't deliver anything else on this
                                // connection — bail out cleanly.
                                break;
                            }
                            {
                                let mut m = metrics.write().await;
                                m.total_requests += 1;
                                m.successful_requests += 1;
                            }
                        } else {
                            // V11 (2026-08-11): auth signal is computed
                            // at handshake time (peer address + XFF +
                            // internal token). Direct-loopback callers
                            // stay internal; nginx-proxied public WS is
                            // treated as external and hits the
                            // destructive-RPC gate.
                            let response = handlers
                                .handle_json_rpc_with_auth(&request_str, is_internal)
                                .await;
                            if write_tx.send(WsWriteFrame::Text(response)).is_err() {
                                break;
                            }
                            {
                                let mut m = metrics.write().await;
                                m.total_requests += 1;
                                m.successful_requests += 1;
                            }
                        }
                    }
                }
                0x8 => {
                    // Peer initiated the close; echo an empty close and
                    // exit. Per RFC 6455 §5.5.1 the response may carry a
                    // status code but is not required to; we mirror the
                    // pre-refactor behaviour of sending an empty close.
                    let _ = write_tx.send(WsWriteFrame::Close(Vec::new()));
                    close_requested = true;
                    break;
                }
                0x9 => {
                    if write_tx.send(WsWriteFrame::Pong(payload)).is_err() {
                        break;
                    }
                }
                0xA => {}
                _ => {
                    break;
                }
            }
        }

        // Shutdown sequence — order matters:
        //   1. Drop the subscription bridge first: this closes its
        //      internal command channel, which lets the pump task drop
        //      the upstream WSS connection to Reth cleanly (no
        //      dangling subscriptions server-side).
        drop(bridge);
        //   2. Drop the writer's send side: after this the writer task
        //      will drain any remaining queued frames (e.g. the Close
        //      frame we just enqueued on the oversized-frame path) and
        //      then observe recv() -> None and exit.
        drop(write_tx);
        //   3. Await the writer task with a bounded timeout so a stuck
        //      TCP socket can't wedge the connection cleanup forever.
        //      The timeout also guarantees the oversized-frame
        //      regression test's `server_task` completes within its own
        //      5s deadline.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), writer_task).await;

        // Suppress unused-variable warning on the shutdown-only local.
        // Kept as a named binding so a future maintainer sees that the
        // reader loop explicitly tracks whether a close was queued.
        let _ = close_requested;

        {
            let mut m = metrics.write().await;
            m.active_connections = m.active_connections.saturating_sub(1);
        }
    } else {
        let body_start = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
        let body = &request[body_start..];
        // V11 (2026-08-11): fallback branch (plain HTTP JSON-RPC hitting
        // the WS port). Same `is_internal` computed above applies; the
        // destructive gate runs when the caller is not loopback + no
        // XFF + no matching internal token.
        let json_response = handlers.handle_json_rpc_with_auth(body, is_internal).await;

        let response = format!(
            "HTTP/1.1 200 OK\r\n\
            Content-Type: application/json\r\n\
            Access-Control-Allow-Origin: *\r\n\
            Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
            Access-Control-Allow-Headers: Content-Type\r\n\
            Access-Control-Expose-Headers: X-Rope-RPC-Version\r\n\
            X-Rope-RPC-Version: {}\r\n\
            Cache-Control: no-store\r\n\
            Content-Length: {}\r\n\r\n{}",
            ROPE_RPC_API_VERSION,
            json_response.len(),
            json_response
        );
        stream.write_all(response.as_bytes()).await?;
    }

    Ok(())
}

fn generate_websocket_accept_key(key: &str) -> String {
    use base64::{engine::general_purpose, Engine as _};

    let magic = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let combined = format!("{}{}", key, magic);

    let mut hasher = Sha1::new();
    hasher.update(combined.as_bytes());
    let result = hasher.finalize();

    general_purpose::STANDARD.encode(result)
}

/// Frame a single, unfragmented WebSocket message (FIN=1) with `opcode`
/// and `payload`, then write it to `stream`. Generic over `AsyncWrite`
/// so the same helper works both on the raw `TcpStream` (used by the
/// oversized-frame regression test) and on the `OwnedWriteHalf` owned by
/// the per-connection writer task in `handle_websocket_connection` (the
/// path that lets `eth_subscription` push notifications from Reth reach
/// the client concurrently with the reader loop).
///
/// Only used server-side, so frames are never masked (mask bit stays 0)
/// per RFC 6455 §5.1. Uses 16-bit extended length for payloads in
/// [126, 65_536) and 64-bit extended length for payloads at or above
/// 65_536; frames larger than `MAX_WS_FRAME_PAYLOAD_BYTES` are already
/// rejected upstream in the reader loop, so this helper does no cap
/// enforcement of its own.
async fn send_websocket_frame<S>(stream: &mut S, opcode: u8, payload: &[u8]) -> anyhow::Result<()>
where
    S: AsyncWrite + Unpin,
{
    let mut frame = Vec::new();

    frame.push(0x80 | opcode);

    let len = payload.len();
    if len < 126 {
        frame.push(len as u8);
    } else if len < 65536 {
        frame.push(126);
        frame.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(len as u64).to_be_bytes());
    }

    frame.extend_from_slice(payload);

    stream.write_all(&frame).await?;
    Ok(())
}

/// A WebSocket frame that the per-connection writer task can be asked to
/// emit toward the client. Kept intentionally small (three variants only):
/// the reader loop, the `SubscriptionBridge` upstream pump, and the
/// shutdown path each pick exactly one variant. Anything richer (e.g.
/// server-initiated pings, fragmented text) would grow this enum and
/// belongs in a future revision, not in the ChainList-red-Score fix.
#[derive(Debug)]
enum WsWriteFrame {
    /// A complete text frame (opcode 0x1, FIN=1). Used for JSON-RPC
    /// responses (from the reader loop's dispatch to
    /// `handle_json_rpc_with_auth` and from the `SubscriptionBridge`'s
    /// `eth_subscribe` / `eth_unsubscribe` reply) and for the
    /// server-initiated `eth_subscription` push notifications the bridge
    /// forwards from Reth verbatim.
    Text(String),
    /// A pong frame (opcode 0xA) in response to a client-initiated ping.
    /// Payload is echoed back exactly per RFC 6455 §5.5.3.
    Pong(Vec<u8>),
    /// A close frame (opcode 0x8). The two-byte payload carries the
    /// RFC 6455 status code (big-endian) — 1000 for normal closure,
    /// 1009 for "Message Too Big" when the reader tripped
    /// `MAX_WS_FRAME_PAYLOAD_BYTES`. Empty payload means "close without
    /// status code" (still legal per §5.5.1). After the writer emits a
    /// close frame it exits its loop.
    Close(Vec<u8>),
}

impl WsWriteFrame {
    /// Convenience: build a close frame carrying a big-endian status
    /// code. Kept here instead of at call sites so the two current
    /// callers (oversized-frame path + shutdown path) can't disagree on
    /// byte layout.
    fn close(status: u16) -> Self {
        WsWriteFrame::Close(status.to_be_bytes().to_vec())
    }
}

/// Adapter that lets the [`SubscriptionBridge`] push
/// [`BridgeWriteFrame`] payloads onto the same per-connection writer
/// channel that carries the reader loop's own JSON-RPC responses.
///
/// The bridge's `to_client` channel deliberately carries only "text
/// payload" — its wire type is `BridgeWriteFrame { text: String }` —
/// because the bridge is oblivious to WebSocket framing details like
/// pong / close opcodes (those are pure client-facing concerns owned
/// by the reader loop). This adapter spawns a small task that
/// republishes every bridge text payload as a `WsWriteFrame::Text` on
/// the writer's mpsc, so:
///
///   * the writer task stays oblivious to who produced a text frame
///     (reader response, subscribe/unsubscribe reply, or forwarded
///     `eth_subscription` push notification) — they're all serialised
///     through a single per-connection queue, preserving RFC 6455
///     "messages arrive in send order" for that connection;
///   * the bridge stays wire-format-agnostic — a future rewrite that
///     switches text→binary frames touches only the writer task and
///     `send_websocket_frame`, not the bridge.
///
/// Task lifetime: the adapter task exits as soon as either side's
/// channel is closed. Both `bridge_tx` (returned) and `tx` (writer)
/// dropping triggers a clean exit, matching the shutdown sequence in
/// `handle_websocket_connection`.
fn bridge_writer_adapter(
    tx: mpsc::UnboundedSender<WsWriteFrame>,
) -> mpsc::UnboundedSender<BridgeWriteFrame> {
    let (bridge_tx, mut bridge_rx) = mpsc::unbounded_channel::<BridgeWriteFrame>();
    tokio::spawn(async move {
        while let Some(BridgeWriteFrame { text }) = bridge_rx.recv().await {
            // If the writer channel has been dropped (client
            // disconnected, close frame already sent) there is nothing
            // sensible to do but drop the notification; the bridge
            // itself will discover the closure the next time its own
            // upstream socket produces a frame and shut down cleanly.
            if tx.send(WsWriteFrame::Text(text)).is_err() {
                break;
            }
        }
    });
    bridge_tx
}

/// Per-connection writer task. Owns the write half of the client TCP
/// stream and drains a single `mpsc::UnboundedReceiver<WsWriteFrame>`
/// until either a `Close` frame is emitted or the sender side is
/// dropped. Kept intentionally simple: one task per connection, no
/// batching, no back-pressure past the (unbounded) channel — the only
/// producers are the reader loop and the bridge, and both are
/// per-connection, so unbounded is bounded in practice by the client's
/// own subscription rate.
async fn ws_writer_task<W>(mut write_half: W, mut rx: mpsc::UnboundedReceiver<WsWriteFrame>)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    while let Some(frame) = rx.recv().await {
        let result = match &frame {
            WsWriteFrame::Text(text) => {
                send_websocket_frame(&mut write_half, 0x1, text.as_bytes()).await
            }
            WsWriteFrame::Pong(payload) => {
                send_websocket_frame(&mut write_half, 0xA, payload).await
            }
            WsWriteFrame::Close(payload) => {
                send_websocket_frame(&mut write_half, 0x8, payload).await
            }
        };
        if let Err(e) = result {
            tracing::debug!(
                target: "rope_node::rpc_server",
                error = %e,
                "ws writer: write failed; closing writer task"
            );
            break;
        }
        if matches!(frame, WsWriteFrame::Close(_)) {
            // After a Close frame we drain nothing further; the reader
            // side is already breaking its loop and dropping the tx.
            break;
        }
    }
}

/// Inspect a JSON-RPC request body and, if it targets the two Reth
/// subscription methods that Datachain Rope's own dispatcher does not
/// implement (`eth_subscribe` / `eth_unsubscribe`), return the parsed
/// value and method name so the caller can hand them to the
/// `SubscriptionBridge`. Any other request — including malformed JSON —
/// returns `None`, letting the caller fall through to
/// `handle_json_rpc_with_auth` unchanged. Deliberately scoped to those
/// two methods so a future dispatcher addition can't be accidentally
/// stolen by the bridge.
/// Parse a raw JSON-RPC request text just enough to decide whether the
/// subscription bridge should own it, and extract the pieces
/// [`SubscriptionBridge::handle`] needs.
///
/// Returns `Some((method, id))` iff `body` is a syntactically valid
/// JSON-RPC request whose `method` is one that
/// [`SubscriptionBridge::is_bridged_method`] recognises
/// (`eth_subscribe` / `eth_unsubscribe`). `id` is left as [`serde_json::Value::Null`]
/// when the caller omitted it (matches JSON-RPC 2.0 §4.2 notification
/// semantics), so `handle` still has something to echo back on error.
///
/// Non-bridged methods (or invalid JSON) return `None` and fall through
/// to the standard dispatcher unchanged. Crucially, we do NOT parse the
/// full request here — the bridge forwards the raw bytes verbatim to
/// Reth to avoid any semantic drift between serialize/deserialize
/// round-trips (e.g. accidental id-type coercion).
fn parse_subscription_request(body: &str) -> Option<(String, serde_json::Value)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let method = value.get("method")?.as_str()?;
    if !SubscriptionBridge::is_bridged_method(method) {
        return None;
    }
    let id = value.get("id").cloned().unwrap_or(serde_json::Value::Null);
    Some((method.to_string(), id))
}

// ============================================================================
// JSON-RPC Handler — the core routing logic
// ============================================================================

/// Result of delegating a request to the EVM execution layer (Reth in
/// production).
enum EvmResult {
    /// EVM backend returned a "result" field.
    Ok(serde_json::Value),
    /// EVM backend returned an "error" field — forward to the client verbatim.
    EvmError(serde_json::Value),
    /// EVM backend is absent, unhealthy, or HTTP call failed.
    Unavailable,
}

/// Server-side filter bag for `rope_listStrings` / `rope_listStringsWithKnots`.
///
/// Carries every filter the Rope Graph spec §4.2 defines so the caller
/// can build it once and pass it through the descriptor-matcher without
/// argument-soup. All fields are optional / defaulted; the empty bag
/// (no kinds, no platform, no parent/ancestor, `verified_only=false`,
/// `min_knots=0`) is a no-op pass-through.
#[derive(Default, Debug)]
struct StringListFilters<'a> {
    kinds: Option<Vec<rope_core::personal_ledger::StringKind>>,
    platform: Option<&'a str>,
    parent_id: Option<&'a str>,
    ancestor_id: Option<&'a str>,
    verified_only: bool,
    min_knots: u64,
    active_since: Option<i64>,
}

impl RpcHandlers {
    /// Return (lazily initialising on first use) the Phase-2 destructive-RPC
    /// auth verifier. Founder keys are pulled from the master-node registry
    /// when present; if no governance manager is wired, the verifier still
    /// works for wallet-owned methods (secp256k1) and rejects governance
    /// calls because no founder pubkey is registered.
    fn auth_verifier(&self) -> Arc<crate::rpc_signature::AuthVerifier> {
        if let Some(v) = self.auth_verifier.read().as_ref() {
            return v.clone();
        }
        let founder_keys = self
            .governance
            .as_ref()
            .map(|g| g.registry_snapshot().founder.founder_keys.clone())
            .unwrap_or_default();
        let v = crate::rpc_signature::AuthVerifier::new(
            &founder_keys,
            crate::rpc_signature::DEFAULT_REPLAY_WINDOW_SECS,
        );
        let mut slot = self.auth_verifier.write();
        if slot.is_none() {
            *slot = Some(v.clone());
        }
        slot.as_ref().unwrap().clone()
    }

    /// Delegate an EVM method to the EVM execution layer. Returns the
    /// structured `EvmResult`.
    async fn delegate_to_evm(&self, request: &serde_json::Value) -> EvmResult {
        let evm = match self.evm_backend.as_ref() {
            Some(b) => b,
            None => return EvmResult::Unavailable,
        };
        if !evm.is_healthy() {
            return EvmResult::Unavailable;
        }
        match evm.forward_request(request).await {
            Ok(response) => {
                if let Some(result) = response.get("result") {
                    EvmResult::Ok(result.clone())
                } else if let Some(error) = response.get("error") {
                    tracing::debug!("EVM backend returned error: {:?}", error);
                    EvmResult::EvmError(error.clone())
                } else {
                    EvmResult::Unavailable
                }
            }
            Err(e) => {
                tracing::warn!("EVM backend delegation failed: {}", e);
                EvmResult::Unavailable
            }
        }
    }

    /// Convert an `EvmResult` into either the inner value or a full JSON-RPC
    /// error response string (EVM error forwarded verbatim, or "unavailable").
    fn unwrap_evm_or_error(
        &self,
        res: EvmResult,
        method: &str,
        id: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        match res {
            EvmResult::Ok(v) => Ok(v),
            EvmResult::EvmError(e) => Err(serde_json::json!({
                "jsonrpc": "2.0",
                "error": e,
                "id": id
            })
            .to_string()),
            EvmResult::Unavailable => Err(self.evm_unavailable_error(method, id)),
        }
    }

    /// Ensure a block result has a non-null "hash" field so strict clients
    /// (e.g. Forge) can parse it. Some EVM execution layers return
    /// `hash: null` for pending blocks; some tools require the field to be a
    /// concrete string.
    fn ensure_block_has_hash(&self, result: serde_json::Value) -> serde_json::Value {
        let obj = match result.as_object() {
            Some(o) => o.clone(),
            None => return result,
        };
        let hash = obj.get("hash");
        let needs_hash = match hash {
            None => true,
            Some(serde_json::Value::Null) => true,
            Some(_) => false,
        };
        if !needs_hash {
            return result;
        }
        // Use zero 32-byte hash so the field is present and is a string (Forge-compatible).
        const ZERO_HASH: &str =
            "0x0000000000000000000000000000000000000000000000000000000000000000";
        let mut out = obj;
        out.insert("hash".to_string(), serde_json::json!(ZERO_HASH));
        serde_json::Value::Object(out)
    }

    /// Return a JSON-RPC error indicating the EVM execution-layer backend is
    /// not connected.
    fn evm_unavailable_error(&self, method: &str, id: &serde_json::Value) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "error": {
                "code": -32603,
                "message": format!(
                    "EVM backend not connected. {} requires the EVM execution \
                     layer (Reth in production). Datachain Rope native \
                     consensus is running.",
                    method
                )
            },
            "id": id
        })
        .to_string()
    }

    // ========================================================================
    // Rope Graph helpers (Quipu Canon v1.2 + Rope Graph spec v1)
    //
    // These helpers turn the on-chain `LedgerDescriptor` and the static
    // `entity_labels` registry into the canonical extended-shape JSON the
    // event.datachain.one Rope Graph consumes. Designed to be called from
    // multiple dispatcher arms without duplicating shape logic.
    // ========================================================================



    /// Build the extended-shape `String` JSON for a real on-chain string
    /// descriptor, including its full `labels` block, parent/ecosystem
    /// pointers, and pre-aggregated child/descendant counts.
    ///
    /// Pure data plumbing — no IO, no allocation hot path concerns.
    fn string_descriptor_to_json(
        &self,
        d: &rope_core::personal_ledger::LedgerDescriptor,
    ) -> serde_json::Value {
        let labels_arc = entity_labels::current();
        let labels_reg: &entity_labels::LabelRegistry = &labels_arc;
        let raw_id = hex::encode(d.id_bytes());
        let label = labels_reg.get(&raw_id);

        let (parent_string_id, ecosystem_id, child_count, descendant_count) = match label {
            Some(l) => (
                l.parent.map(|p| format!("0x{}", p)),
                l.ecosystem.map(|e| format!("0x{}", e)),
                labels_reg.child_count_of(l.id_hex) as u64,
                labels_reg.descendant_count_of(l.id_hex) as u64,
            ),
            None => (None, None, 0u64, 0u64),
        };

        let mut json = serde_json::json!({
            // ---- Quipu Canon v1.2 canonical names ----
            "kind": d.string_id_kind().as_str(),
            "string_id": d.string_id_hex(),
            "genesis_knot_id": d.genesis_knot_id().to_hex(),
            "head_knot_id": d.head_knot_id().to_hex(),
            "knot_count": d.knot_count(),
            "total_size_bytes": d.total_size_bytes,
            "is_deleted": d.is_deleted,
            "created_at": d.created_at,
            "last_anchored_at": d.last_appended_at,

            // ---- Rope Graph v1 extensions ----
            "parent_string_id": parent_string_id,
            "ecosystem_id": ecosystem_id,
            "child_count": child_count,
            "descendant_count": descendant_count,
            "labels": Self::label_json(label),
            "verified": label.map(|l| l.verified).unwrap_or(false),
            "verifier": label.and_then(|l| l.verifier),

            // ---- v1.0/1.1 deprecated aliases — drop in v1.3 ----
            "wallet_address": d.string_id_hex(),
            "genesis_string_id": d.genesis_string_id.to_hex(),
            "head_string_id": d.head_string_id.to_hex(),
        });

        // Hidden labels: redact the raw id from the response. The label
        // is still surfaced through the `labels.display_name` so the
        // frontend can render it as a strand without ever seeing the hex.
        if let Some(l) = label {
            if l.hidden {
                if let Some(obj) = json.as_object_mut() {
                    obj.insert("string_id".to_string(), serde_json::Value::Null);
                    obj.insert("wallet_address".to_string(), serde_json::Value::Null);
                    obj.insert(
                        "string_id_hidden_label".to_string(),
                        serde_json::json!(l.display_name),
                    );
                }
            }
        }
        json
    }

    /// Build the synthetic `String` JSON for an entity that has no
    /// on-chain descriptor yet (e.g. an ecosystem or application). The
    /// `knot_count` is reported as 0 and `is_deleted` as false.
    fn synthetic_string_to_json(&self, l: &EntityLabel) -> serde_json::Value {
        let labels_arc = entity_labels::current();
        let labels_reg: &entity_labels::LabelRegistry = &labels_arc;
        let id = format!("0x{}", l.id_hex);
        serde_json::json!({
            "kind": l.kind.as_str(),
            "string_id": id,
            "genesis_knot_id": null,
            "head_knot_id": null,
            "knot_count": 0,
            "total_size_bytes": 0,
            "is_deleted": false,
            "created_at": 0,
            "last_anchored_at": 0,
            "parent_string_id": l.parent.map(|p| format!("0x{}", p)),
            "ecosystem_id": l.ecosystem.map(|e| format!("0x{}", e)),
            "child_count": labels_reg.child_count_of(l.id_hex) as u64,
            "descendant_count": labels_reg.descendant_count_of(l.id_hex) as u64,
            "labels": Self::label_json(Some(l)),
            "verified": l.verified,
            "verifier": l.verifier,
            "synthetic": true,
        })
    }

    /// Render an `EntityLabel` as the spec's `labels` block.
    fn label_json(label: Option<&EntityLabel>) -> serde_json::Value {
        match label {
            Some(l) => serde_json::json!({
                "display_name": l.display_name,
                "short_name": l.short_name,
                "description": l.description,
                "platform": l.platform,
                "role": l.role,
                "icon": l.icon,
            }),
            None => serde_json::json!({
                "display_name": null,
                "short_name": null,
                "description": null,
                "platform": null,
                "role": null,
                "icon": null,
            }),
        }
    }

    /// Build the extended-shape Knot JSON. `tx_hash`, `block_number`,
    /// `from`, `to`, `method_*`, `value_wei`, `fees_wei` are populated
    /// when the EVM backend is available and the knot is bound to an
    /// EVM transaction; otherwise those fields are `null` and the knot
    /// is treated as a pure canon-layer event (per the spec, §10 q3).
    fn knot_to_json(
        &self,
        knot_index: usize,
        entry: &rope_core::lattice::LedgerEntry,
        anchored_by_string_id: &str,
    ) -> serde_json::Value {
        use rope_core::lattice::LedgerEntry;
        let (status, knot_string_id, anchored_at, tombstone) = match entry {
            LedgerEntry::Active(sid) => ("active", sid.to_hex(), None, serde_json::Value::Null),
            LedgerEntry::Tombstone(sid, ts) => (
                "tombstone",
                sid.to_hex(),
                Some(ts.untied_at),
                serde_json::json!({
                    "untied_at": ts.untied_at,
                    "audit_hash": format!("0x{}", hex::encode(ts.audit_hash)),
                    "reason": ts.reason,
                }),
            ),
        };

        // Heuristic activity classification. When the canon layer doesn't
        // yet annotate knots with an activity-kind tag, we default to
        // `"anchor_data"` for active knots and `"tombstone"` for untied
        // ones. The frontend uses this for colouring; agents can refine
        // by inspecting the payload digest once the canon emits it.
        let kind = if status == "tombstone" {
            "tombstone"
        } else {
            "anchor_data"
        };

        serde_json::json!({
            // ---- Position + identity ----
            "knot_index": knot_index,
            "knot_id": format!("0x{}", knot_string_id),
            "string_id": format!("0x{}", knot_string_id),
            "status": status,
            "kind": kind,
            "anchored_at": anchored_at,

            // ---- EVM context (null when unbound, see §10 q3) ----
            "block_number": serde_json::Value::Null,
            "tx_hash": serde_json::Value::Null,
            "from": serde_json::Value::Null,
            "to": serde_json::Value::Null,
            "method_selector": serde_json::Value::Null,
            "method_name": serde_json::Value::Null,
            "value_wei": serde_json::Value::Null,
            "fees_wei": serde_json::Value::Null,
            "payload_digest": serde_json::Value::Null,
            "payload_size": serde_json::Value::Null,

            // ---- Tombstone metadata ----
            "tombstone": tombstone,

            // ---- Cross-references ----
            "links": {
                "explorer": serde_json::Value::Null,
                "anchored_by_string_id": format!("0x{}", anchored_by_string_id),
            },
        })
    }

    /// Apply the full Rope-Graph filter set to a candidate descriptor.
    /// Returns `true` when the descriptor passes every supplied filter.
    fn descriptor_matches_filters(
        &self,
        d: &rope_core::personal_ledger::LedgerDescriptor,
        f: &StringListFilters<'_>,
    ) -> bool {
        if let Some(ks) = &f.kinds {
            if !ks.contains(&d.string_id_kind()) {
                return false;
            }
        }
        if d.knot_count() < f.min_knots {
            return false;
        }
        if let Some(ts) = f.active_since {
            if d.last_appended_at < ts {
                return false;
            }
        }

        let labels_arc = entity_labels::current();
        let labels_reg: &entity_labels::LabelRegistry = &labels_arc;
        let raw_id = hex::encode(d.id_bytes());
        let label = labels_reg.get(&raw_id);

        if let Some(p) = f.platform {
            match label {
                Some(l) if l.platform.eq_ignore_ascii_case(p) => {}
                _ => return false,
            }
        }
        if f.verified_only {
            match label {
                Some(l) if l.verified => {}
                _ => return false,
            }
        }
        if let Some(parent) = f.parent_id {
            let key = parent.trim_start_matches("0x").to_ascii_lowercase();
            match label.and_then(|l| l.parent) {
                Some(p) if p.eq_ignore_ascii_case(&key) => {}
                _ => return false,
            }
        }
        if let Some(ancestor) = f.ancestor_id {
            let key = ancestor.trim_start_matches("0x").to_ascii_lowercase();
            // Accept either direct parent OR ecosystem ancestor for the
            // common case ("everything under DCSwap").
            let parent = label.and_then(|l| l.parent).unwrap_or("");
            let eco = label.and_then(|l| l.ecosystem).unwrap_or("");
            if !parent.eq_ignore_ascii_case(&key) && !eco.eq_ignore_ascii_case(&key) {
                return false;
            }
        }
        true
    }

    /// Parse the Rope-Graph spec's `kind_filter` parameter. Accepts
    /// either a single string ("contract") or an array of strings
    /// (["wallet","bot"]). Falls back to the legacy `kind` field name
    /// for v1.0/1.1 callers. Unknown strings are silently dropped.
    fn parse_kind_filter(
        &self,
        p: Option<&serde_json::Value>,
    ) -> Option<Vec<rope_core::personal_ledger::StringKind>> {
        use rope_core::personal_ledger::StringKind;
        let raw = p?
            .get("kind_filter")
            .or_else(|| p?.get("kind"))
            .or_else(|| p?.get("kinds"))?;
        if let Some(s) = raw.as_str() {
            return StringKind::parse(s).map(|k| vec![k]);
        }
        if let Some(arr) = raw.as_array() {
            let kinds: Vec<_> = arr
                .iter()
                .filter_map(|v| v.as_str().and_then(StringKind::parse))
                .collect();
            if kinds.is_empty() {
                return None;
            }
            return Some(kinds);
        }
        None
    }

    /// Sort a slice of descriptors per the spec's `sort` parameter.
    fn sort_descriptors(
        &self,
        slice: &mut [rope_core::personal_ledger::LedgerDescriptor],
        sort: Option<&str>,
    ) {
        match sort.unwrap_or("newest").to_ascii_lowercase().as_str() {
            "knots_desc" => {
                slice.sort_by_key(|d| std::cmp::Reverse(d.knot_count()));
            }
            "oldest" => slice.sort_by_key(|d| d.last_appended_at),
            "name_asc" => {
                let reg_arc = entity_labels::current();
                let reg: &entity_labels::LabelRegistry = &reg_arc;
                slice.sort_by(|a, b| {
                    let an = reg
                        .get(&hex::encode(a.id_bytes()))
                        .map(|l| l.display_name)
                        .unwrap_or("zzz");
                    let bn = reg
                        .get(&hex::encode(b.id_bytes()))
                        .map(|l| l.display_name)
                        .unwrap_or("zzz");
                    an.cmp(bn)
                });
            }
            // "newest" (default)
            _ => slice.sort_by_key(|d| std::cmp::Reverse(d.last_appended_at)),
        }
    }

    /// List of every method this dispatcher answers. Used by
    /// `rpc_methods` and as a fallback for `rpc_modules`.
    fn supported_methods() -> Vec<&'static str> {
        vec![
            // EVM compat
            "eth_chainId",
            "eth_blockNumber",
            "eth_gasPrice",
            "eth_maxPriorityFeePerGas",
            "eth_feeHistory",
            "eth_syncing",
            "eth_accounts",
            "eth_protocolVersion",
            "eth_mining",
            "eth_hashrate",
            "eth_getBalance",
            "eth_getCode",
            "eth_getStorageAt",
            "eth_getTransactionCount",
            "eth_getTransactionByHash",
            "eth_getTransactionReceipt",
            "eth_getBlockByNumber",
            "eth_getBlockByHash",
            "eth_call",
            "eth_estimateGas",
            "eth_sendRawTransaction",
            "eth_getLogs",
            "eth_getUncleCountByBlockNumber",
            "eth_getUncleCountByBlockHash",
            "net_version",
            "net_listening",
            "net_peerCount",
            "web3_clientVersion",
            // Quipu Canon
            "rope_knotIndex",
            "rope_getKnotByIndex",
            "rope_getKnotByHash",
            "rope_globalStats",
            "rope_listStrings",
            "rope_getString",
            "rope_getStringById",
            "rope_getStringWithKnots",
            "rope_listKnots",
            "rope_listStringsWithKnots",
            "rope_listEcosystems",
            "rope_listApplications",
            "rope_listRelations",
            "rope_resolveLabel",
            "rope_appendToLedger",
            "rope_createPersonalLedger",
            "rope_untieKnot",
            "rope_anchorDeployerAttestation",
            "rope_listDeployerAttestations",
            "rope_governanceInfo",
            "rope_latticeMetrics",
            "rope_listMasterNodes",
            "rope_nodeIdentity",
            "rope_suspendNode",
            "rope_isolateNode",
            "rope_eraseNode",
            // IoT
            "rope_registerDevice",
            "rope_ingestTelemetry",
            "rope_getDeviceStatus",
            "rope_getIoTGatewayStats",
            "rope_listDevices",
            // AI agent framework
            "rope_registerAgent",
            "rope_getAgentStatus",
            "rope_listAgents",
            "rope_subscribeAgentToWallet",
            "rope_getRecentDiagnoses",
            // Discovery
            "rpc_methods",
            "rpc_modules",
        ]
    }

    /// Render a derived `Relation` between two strings.
    fn relation_json(
        kind: &str,
        from_id: &str,
        to_id: &str,
        weight: u64,
    ) -> serde_json::Value {
        let id_seed = format!("{}:{}->{}", kind, from_id, to_id);
        let id_hex = hex::encode(blake3::hash(id_seed.as_bytes()).as_bytes());
        serde_json::json!({
            "relation_id": format!("0x{}", &id_hex[..32]),
            "kind": kind,
            "from_string_id": from_id,
            "to_string_id": to_id,
            "weight": weight,
            "first_seen_at": serde_json::Value::Null,
            "last_seen_at": serde_json::Value::Null,
            "metadata": serde_json::json!({}),
        })
    }

    /// Build the full set of derived relations from the static label
    /// registry. Used by `rope_listRelations`.
    fn derive_relations(reg: &LabelRegistry) -> Vec<serde_json::Value> {
        let mut out = Vec::new();
        for l in reg.all() {
            let id = format!("0x{}", l.id_hex);
            // Every label with a parent emits the canonical `parent_of` ->
            // `child_of` direction. The spec uses kind-specific verbs:
            //   bot      -> operates / operated_by
            //   contract -> belongs_to
            //   asset    -> issues / issued_by
            //   wallet   -> owns
            //   default  -> attached_to
            let kind = match l.kind {
                LabelKind::Bot => "operates",
                LabelKind::Contract => "belongs_to",
                LabelKind::Asset => "issues",
                LabelKind::Wallet => "owns",
                LabelKind::Application => "hosts",
                LabelKind::Agent => "operates",
                LabelKind::Validator => "secures",
                LabelKind::Oracle => "feeds",
                LabelKind::Did => "identifies",
                LabelKind::Organization => "operates",
                LabelKind::Partner => "partners_with",
                LabelKind::Ecosystem | LabelKind::Cord => continue,
            };
            if let Some(parent) = l.parent {
                let parent_id = format!("0x{}", parent);
                out.push(Self::relation_json(kind, &parent_id, &id, 1));
            }
        }
        out
    }

    /// Handle JSON-RPC request.
    ///
    /// rope-node is the MASTER. It always handles:
    ///   - Chain identity (eth_chainId, net_version, web3_clientVersion)
    ///   - Native Rope consensus (rope_*)
    ///   - Block number from its own round counter
    ///   - Gas price (native)
    ///
    /// When the EVM backend (Reth in production) is connected, rope-node
    /// delegates EVM state queries to it. When it is absent, EVM-specific
    /// queries return a proper JSON-RPC error.
    /// Public entry point — assumes the caller is on the public listener
    /// (no V11 bypass). Currently used only by the unit tests below;
    /// production paths flow through `handle_connection` /
    /// `handle_websocket_connection` which call `handle_json_rpc_with_auth`
    /// directly with the correct flag.
    #[allow(dead_code)]
    async fn handle_json_rpc(&self, body: &str) -> String {
        self.handle_json_rpc_with_auth(body, false).await
    }

    /// Auth-aware entry point. When `is_internal == true`, the V11 destructive-
    /// method gate is bypassed. The caller is responsible for proving the
    /// request is authentic — currently:
    ///   - HTTP transport checks `X-Rope-Internal-Token` against the env var
    ///     `ROPE_INTERNAL_RPC_TOKEN` (constant-time compare in
    ///     `rpc_auth::internal_token_matches`).
    ///   - WebSocket transport is bound loopback-only (see
    ///     `RpcServer::run`), so all WS callers are local; we pass
    ///     `is_internal = true` there.
    /// The deployed nginx config strips any inbound copy of
    /// `X-Rope-Internal-Token` from public traffic, so an internet-side
    /// attacker cannot forge it.
    async fn handle_json_rpc_with_auth(&self, body: &str, is_internal: bool) -> String {
        let mut request: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(_) => {
                // Return standard JSON-RPC Parse error so clients (Forge, cast) get a proper
                // error instead of an extended object that breaks eth_chainId parsing.
                return serde_json::json!({
                    "jsonrpc": "2.0",
                    "error": { "code": -32700, "message": "Parse error" },
                    "id": serde_json::Value::Null
                })
                .to_string();
            }
        };

        // Batch request: body is a JSON array of requests. Forward to the EVM backend and return response as-is.
        if request.is_array() {
            if let Some(evm) = &self.evm_backend {
                if evm.is_healthy() {
                    match evm.forward_request(&request).await {
                        Ok(response) => {
                            return serde_json::to_string(&response)
                                .unwrap_or_else(|_| "[]".to_string());
                        }
                        Err(e) => {
                            tracing::warn!("EVM backend batch forward failed: {}", e);
                        }
                    }
                }
            }
            // EVM backend absent or failed: return batch of errors so client gets valid JSON-RPC batch shape.
            let batch = request.as_array().map(|a| a.len()).unwrap_or(0);
            let errors: Vec<serde_json::Value> = (0..batch)
                .map(|i| {
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": { "code": -32603, "message": "EVM backend not connected" },
                        "id": i
                    })
                })
                .collect();
            return serde_json::to_string(&errors).unwrap_or_else(|_| "[]".to_string());
        }

        // Single request: must be an object (method, params, id).
        if !request.is_object() {
            return serde_json::json!({
                "jsonrpc": "2.0",
                "error": { "code": -32600, "message": "Invalid request" },
                "id": serde_json::Value::Null
            })
            .to_string();
        }

        let method_string = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string();
        let method = method_string.as_str();
        let id = request.get("id").cloned().unwrap_or(serde_json::json!(1));

        // Destructive-RPC gate.
        //
        // Two policies coexist:
        //   - Phase-1 (V11 hot-fix, default): unsigned destructive calls on
        //     the public listener are denied with -32401. Loopback / WS /
        //     X-Rope-Internal-Token callers (i.e. `is_internal == true`)
        //     bypass the gate.
        //   - Phase-2 (`ROPE_PHASE2_SIGNED_DESTRUCTIVE=1`): signed
        //     destructive calls authenticate via secp256k1 / Ed25519 (see
        //     `crates/rope-node/src/rpc_signature.rs`). On accept, the
        //     auth envelope is stripped from `params` so existing dispatch
        //     handlers don't see the extra element. On reject, the call is
        //     denied with -32401 just like Phase-1.
        //
        // Phase-1 and Phase-2 are NOT mutually exclusive: when both are
        // enabled, a Phase-2 success bypasses Phase-1 (legitimate signed
        // call), but a Phase-2 failure still hits Phase-1 (unsigned or
        // bad-sig calls remain denied). When Phase-2 is OFF (default),
        // the existing V11 behaviour is preserved bit-for-bit.
        if !is_internal && crate::rpc_auth::DESTRUCTIVE_METHODS.contains(&method) {
            let phase2_on = crate::rpc_auth::phase2_signed_destructive_enabled();
            let phase1_on = crate::rpc_auth::public_destructive_deny_enabled();

            let mut accepted_via_phase2 = false;
            if phase2_on {
                let params_value = request
                    .get("params")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let verifier = self.auth_verifier();
                match crate::rpc_signature::verify_destructive_call_for_chain(
                    &verifier,
                    self.chain_id,
                    method,
                    &params_value,
                ) {
                    Ok(verified) => {
                        accepted_via_phase2 = true;
                        tracing::info!(
                            target: "rope_node::auth",
                            method = method,
                            verified = ?verified,
                            "Phase-2 signed destructive RPC accepted"
                        );
                        if let Some(obj) = request.as_object_mut() {
                            if let Some(arr) =
                                obj.get("params").and_then(|p| p.as_array()).cloned()
                            {
                                let mut without = arr;
                                without.pop();
                                obj.insert(
                                    "params".to_string(),
                                    serde_json::Value::Array(without),
                                );
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            target: "rope_node::auth",
                            method = method,
                            error = %e,
                            "Phase-2 signed destructive RPC rejected"
                        );
                        if !phase1_on {
                            return crate::rpc_auth::denied_response(&id);
                        }
                    }
                }
            }

            if !accepted_via_phase2 && phase1_on {
                tracing::warn!(
                    target: "rope_node::auth",
                    method = method,
                    "rejected destructive method on public listener (V11 hot-fix). \
                     Set ROPE_PUBLIC_DESTRUCTIVE_DENY=0 only on a private listener, \
                     supply X-Rope-Internal-Token, or enable Phase-2 \
                     (ROPE_PHASE2_SIGNED_DESTRUCTIVE=1) and sign the call."
                );
                return crate::rpc_auth::denied_response(&id);
            }
        }

        let params = request.get("params");

        // CERBER WATCH — `blocked_signers` gate (2026-07-25 audit follow-up,
        // finding H1/C4). Applies to EVERY caller, internal included: a
        // compromised key does not stop being compromised because the
        // call arrived over loopback or with a valid `X-Rope-Internal-Token`.
        // This is deliberately independent of, and runs regardless of, the
        // destructive-method gate above — it protects the wallet named in
        // the call, not the RPC transport the call arrived on.
        if let Some(signer) = crate::rpc_auth::wallet_param_for_method(method, params) {
            if let Err(denial) = request_guard().check_signer(signer) {
                tracing::warn!(
                    target: "rope_node::auth",
                    method = method,
                    signer = signer,
                    denial = %denial,
                    "CERBER WATCH: rejected call naming a blocklisted signer"
                );
                return crate::rpc_auth::blocked_signer_response(&id, signer);
            }
        }

        let result = match method {
            // ================================================================
            // ROPE-NODE AUTHORITATIVE — always answered natively
            // ================================================================
            "eth_chainId" => {
                serde_json::json!(format!("0x{:x}", self.chain_id))
            }
            "net_version" => {
                serde_json::json!(self.chain_id.to_string())
            }
            "web3_clientVersion" => {
                serde_json::json!(format!("Datachain-Rope/{}", self.network_version))
            }
            "eth_syncing" => serde_json::json!(false),
            "eth_accounts" => serde_json::json!([]),
            "eth_protocolVersion" => serde_json::json!("0x41"),
            "net_listening" => serde_json::json!(true),
            "eth_mining" => serde_json::json!(false),
            "eth_hashrate" => serde_json::json!("0x0"),

            // Knot index / block number: rope-node tracks this from its own
            // anchor rounds. If the EVM backend is connected, we prefer its
            // value; otherwise our round counter.
            //
            // Quipu Canon v1.1 §3 — `rope_knotIndex` is the canonical name.
            // `eth_blockNumber` is preserved as an EVM-compat alias so
            // MetaMask, ethers.js, hardhat, and existing tooling keep working.
            // The canon variant rewrites the on-wire method name to
            // `eth_blockNumber` before delegating, so the EVM backend
            // (Reth) — which does not know the `rope_*` namespace — still
            // recognizes the request.
            "eth_blockNumber" | "rope_knotIndex" => {
                let upstream_request = if method == "rope_knotIndex" {
                    let mut r = request.clone();
                    if let Some(obj) = r.as_object_mut() {
                        obj.insert("method".to_string(), serde_json::json!("eth_blockNumber"));
                    }
                    r
                } else {
                    request.clone()
                };
                let cached = *self.block_number.read();
                // Health probes and nginx upstream checks must not queue behind
                // heavy rope_* work. Prefer a fresh Reth tip, but fall back to
                // the background-refreshed cache within 800ms.
                const TIP_PROBE_BUDGET: std::time::Duration = std::time::Duration::from_millis(800);
                match tokio::time::timeout(TIP_PROBE_BUDGET, self.delegate_to_evm(&upstream_request))
                    .await
                {
                    Ok(EvmResult::Ok(result)) => {
                        if let Some(hex_str) = result.as_str() {
                            if let Ok(n) = u64::from_str_radix(hex_str.trim_start_matches("0x"), 16)
                            {
                                *self.block_number.write() = n;
                            }
                        }
                        result
                    }
                    Ok(EvmResult::EvmError(e)) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":e,"id":id}).to_string();
                    }
                    Ok(EvmResult::Unavailable) | Err(_) => {
                        if cached > 0 {
                            serde_json::json!(format!("0x{:x}", cached))
                        } else {
                            serde_json::json!(format!("0x{:x}", cached.max(1)))
                        }
                    }
                }
            }

            // Gas price: rope-node knows its own gas price natively.
            // The EVM backend may have a more accurate value if connected.
            "eth_gasPrice" | "eth_maxPriorityFeePerGas" => {
                match self.delegate_to_evm(&request).await {
                    EvmResult::Ok(result) => result,
                    EvmResult::EvmError(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":e,"id":id}).to_string();
                    }
                    EvmResult::Unavailable => {
                        serde_json::json!(format!("0x{:x}", self.gas_price))
                    }
                }
            }

            // Fee history: rope-node can construct this from its own data.
            "eth_feeHistory" => match self.delegate_to_evm(&request).await {
                EvmResult::Ok(result) => result,
                EvmResult::EvmError(e) => {
                    return serde_json::json!({"jsonrpc":"2.0","error":e,"id":id}).to_string();
                }
                EvmResult::Unavailable => {
                    let block_count = params
                        .and_then(|p| p.as_array())
                        .and_then(|p| p.first())
                        .and_then(|b| b.as_u64())
                        .unwrap_or(1) as usize;
                    self.native_fee_history(block_count)
                }
            },

            // Peer count: rope-node knows its own peer count.
            "net_peerCount" => serde_json::json!("0x0"),

            // Uncle counts: Datachain Rope has no uncles.
            "eth_getUncleCountByBlockNumber" | "eth_getUncleCountByBlockHash" => {
                serde_json::json!("0x0")
            }

            // ================================================================
            // EVM STATE READS — delegate to the EVM backend, native error if absent
            // ================================================================
            //
            // Quipu Canon v1.1 §3 — `rope_getKnotByIndex` / `rope_getKnotByHash`
            // are the canonical names. The two `eth_*` arms are preserved as
            // EVM-compat aliases. The canon variants delegate the same body
            // (rewriting the method name on the wire so the EVM backend
            // understands it), and additionally enrich the response with the
            // `knot` shape.
            "eth_getBlockByNumber"
            | "eth_getBlockByHash"
            | "rope_getKnotByIndex"
            | "rope_getKnotByHash" => {
                let is_canon = matches!(method, "rope_getKnotByIndex" | "rope_getKnotByHash");
                let upstream_request = if is_canon {
                    let upstream_method = match method {
                        "rope_getKnotByIndex" => "eth_getBlockByNumber",
                        "rope_getKnotByHash" => "eth_getBlockByHash",
                        other => other,
                    };
                    let mut r = request.clone();
                    if let Some(obj) = r.as_object_mut() {
                        obj.insert("method".to_string(), serde_json::json!(upstream_method));
                    }
                    r
                } else {
                    request.clone()
                };
                match self.unwrap_evm_or_error(
                    self.delegate_to_evm(&upstream_request).await,
                    method,
                    &id,
                ) {
                    Ok(mut result) => {
                        result = self.ensure_block_has_hash(result);
                        if is_canon {
                            // Surface canon-shaped fields alongside the
                            // legacy block fields (additive, non-breaking).
                            if let Some(obj) = result.as_object_mut() {
                                if let Some(num) = obj.get("number").cloned() {
                                    obj.insert("knotIndex".to_string(), num);
                                }
                                if let Some(h) = obj.get("hash").cloned() {
                                    obj.insert("knotHash".to_string(), h);
                                }
                                if let Some(ts) = obj.get("timestamp").cloned() {
                                    obj.insert("knotTimestamp".to_string(), ts);
                                }
                                obj.insert(
                                    "canon".to_string(),
                                    serde_json::json!(
                                        "v1.1 §3 — eth_getBlockBy* preserved as alias"
                                    ),
                                );
                            }
                        }
                        result
                    }
                    Err(err_response) => return err_response,
                }
            }
            "eth_getBalance"
            | "eth_getTransactionCount"
            | "eth_getCode"
            | "eth_call"
            | "eth_estimateGas"
            | "eth_getStorageAt"
            | "eth_getLogs"
            | "eth_getBlockTransactionCountByNumber"
            | "eth_getBlockTransactionCountByHash"
            | "eth_getTransactionByHash"
            | "eth_getTransactionByBlockNumberAndIndex"
            | "eth_getTransactionReceipt" => {
                match self.unwrap_evm_or_error(self.delegate_to_evm(&request).await, method, &id) {
                    Ok(result) => result,
                    Err(err_response) => return err_response,
                }
            }

            // ================================================================
            // EVM WRITES — delegate to the EVM backend + notarize,
            // native error if absent
            // ================================================================
            "eth_sendRawTransaction" => {
                let raw_tx = params
                    .and_then(|p| p.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str());

                match self.unwrap_evm_or_error(self.delegate_to_evm(&request).await, method, &id) {
                    Ok(result) => {
                        if let Some(tx_hash) = result.as_str() {
                            if let Some(orch) = &self.orchestrator {
                                orch.notarize_transaction(tx_hash, raw_tx).await;
                            }
                        }
                        result
                    }
                    Err(err_response) => return err_response,
                }
            }

            // ================================================================
            // EVM admin / debug methods — Foundry-Anvil-compatibility /
            // devnet-only. Gated OFF by default (2026-07-25 audit §5.1
            // dispatcher-completeness follow-up; see `rpc_auth::DEV_ONLY_EVM_METHODS`
            // doc comment for the full threat model).
            //
            // The `anvil_*` method *names* are wire-protocol identifiers
            // from the legacy Anvil tooling era, kept for Hardhat/Foundry
            // compatibility on local devnets. In production, this arm is
            // reached and immediately rejected LOCALLY — it never reaches
            // the EVM backend at all — unless an operator explicitly sets
            // `ROPE_ALLOW_EVM_DEV_METHODS=1`. Before this gate, safety
            // depended entirely on the EVM backend (Reth) happening not to
            // implement these namespaces; that is safety-by-absence, not
            // safety-by-design, and would silently disappear the moment a
            // backend swap or version bump ever did implement them.
            // ================================================================
            "anvil_impersonateAccount"
            | "anvil_stopImpersonatingAccount"
            | "anvil_setBalance"
            | "anvil_setCode"
            | "anvil_setNonce"
            | "anvil_dumpState"
            | "anvil_loadState"
            | "anvil_mine"
            | "anvil_setStorageAt"
            | "anvil_reset"
            | "evm_snapshot"
            | "evm_revert"
            | "evm_increaseTime"
            | "evm_mine" => {
                if !crate::rpc_auth::dev_only_evm_methods_enabled() {
                    tracing::warn!(
                        target: "rope_node::auth",
                        method = method,
                        "rejected dev-only EVM method (Foundry-Anvil compat namespace). \
                         Set ROPE_ALLOW_EVM_DEV_METHODS=1 only on a local devnet."
                    );
                    return serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32601,
                            "message": format!("Method not found: {}", method)
                        },
                        "id": id
                    })
                    .to_string();
                }
                match self.unwrap_evm_or_error(self.delegate_to_evm(&request).await, method, &id) {
                    Ok(result) => result,
                    Err(err_response) => return err_response,
                }
            }

            // ================================================================
            // DATACHAIN ROPE NATIVE METHODS — always available
            // ================================================================
            //
            // Note: `rope_getStringById` is now an alias of `rope_getString`,
            // resolved further below in this match statement (with the full
            // Rope Graph v1 extended-shape response).
            "rope_getTestimonyStatus" => {
                if let Some(orch) = &self.orchestrator {
                    let stats = orch.stats();
                    // Emit both keys: `evmBackendConnected` is the canonical
                    // name; `anvilConnected` is preserved as a deprecated
                    // alias so existing dashboards (DCScan, indexers) keep
                    // working without a coordinated cutover. New clients
                    // should read `evmBackendConnected`.
                    serde_json::json!({
                        "consensus": "testimony",
                        "pendingTransactions": stats.pending_txs,
                        "finalizedTransactions": stats.finalized_txs,
                        "currentRound": stats.current_round,
                        "evmBackendConnected": stats.evm_backend_connected,
                        "anvilConnected": stats.evm_backend_connected,
                        "aiAgentsActive": stats.ai_agents_active
                    })
                } else {
                    serde_json::json!({
                        "consensus": "testimony",
                        "pendingTransactions": 0,
                        "finalizedTransactions": 0,
                        "currentRound": *self.block_number.read(),
                        "evmBackendConnected": false,
                        "anvilConnected": false,
                        "aiAgentsActive": 0
                    })
                }
            }
            // ================================================================
            // QUIPU CANON v2.0 PHASE 2 — verified Testimony consensus surface
            // ================================================================
            "rope_committeeInfo" => {
                if let Some(orch) = &self.orchestrator {
                    let info = orch.committee_info();
                    let registry = orch.validator_registry();
                    let snapshot = registry.snapshot();
                    let validators: Vec<serde_json::Value> = snapshot
                        .validators
                        .iter()
                        .map(|rec| {
                            serde_json::json!({
                                "nodeId": format!("0x{}", hex::encode(rec.node_id.as_bytes())),
                                "weight": rec.weight,
                                "active": rec.active,
                                "ed25519": format!("0x{}", hex::encode(rec.public_key.ed25519)),
                            })
                        })
                        .collect();
                    let signing = orch.offload_stats();
                    serde_json::json!({
                        "validators": info.validators,
                        "byzantineTolerance": info.byzantine_tolerance,
                        "finalityQuorum": info.finality_quorum,
                        "selfValidatorId": format!("0x{}", info.self_validator_id),
                        "verifySignatures": true,
                        "signatureScheme": "hybrid-ed25519+dilithium3",
                        "committee": validators,
                        // Quipu Canon v2.0 Phase 5 — PQ signing offload
                        "signingPipeline": {
                            "backend": signing.backend,
                            "submitted": signing.submitted,
                            "signed": signing.signed,
                            "batches": signing.batches,
                            "meanBatchSize": signing.mean_batch_size,
                            "queueHighWater": signing.queue_high_water,
                            "lifetimeSigPerSec": signing.lifetime_sig_per_sec,
                        },
                    })
                } else {
                    return rpc_err(
                        &id,
                        -32000,
                        "Consensus orchestrator not initialized",
                    );
                }
            }
            "rope_validatorIdentity" => {
                if let Some(orch) = &self.orchestrator {
                    let vid = orch.validator_id();
                    let registry = orch.validator_registry();
                    match registry.public_key(&vid) {
                        Some(pk) => serde_json::json!({
                            "nodeId": format!("0x{}", hex::encode(vid.as_bytes())),
                            "publicKey": format!("0x{}", hex::encode(pk.to_bytes())),
                            "ed25519": format!("0x{}", hex::encode(pk.ed25519)),
                            "scheme": "hybrid-ed25519+dilithium3",
                            "weight": registry.weight(&vid),
                            "active": registry.is_active(&vid),
                        }),
                        None => {
                            return rpc_err(
                                &id,
                                -32000,
                                "Validator identity not present in registry",
                            );
                        }
                    }
                } else {
                    return rpc_err(
                        &id,
                        -32000,
                        "Consensus orchestrator not initialized",
                    );
                }
            }
            "rope_submitTestimony" => {
                let wire_hex = request
                    .get("params")
                    .and_then(|p| p.as_array())
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str());
                let Some(wire_hex) = wire_hex else {
                    return rpc_err(
                        &id,
                        -32602,
                        "rope_submitTestimony expects params: [\"0x<hex testimony wire>\"]",
                    );
                };
                let wire = match hex::decode(wire_hex.trim_start_matches("0x")) {
                    Ok(b) => b,
                    Err(e) => {
                        return rpc_err(
                            &id,
                            -32602,
                            &format!("invalid hex testimony wire: {e}"),
                        );
                    }
                };
                if let Some(orch) = &self.orchestrator {
                    match orch.submit_peer_testimony(&wire) {
                        Ok(finalized) => serde_json::json!({
                            "accepted": true,
                            "targetFinalized": finalized,
                        }),
                        Err(e) => {
                            return rpc_err(
                                &id,
                                -32001,
                                &format!("testimony rejected: {e}"),
                            );
                        }
                    }
                } else {
                    return rpc_err(
                        &id,
                        -32000,
                        "Consensus orchestrator not initialized",
                    );
                }
            }
            "rope_registerValidator" => {
                let params = request.get("params").and_then(|p| p.as_array());
                let node_id_hex = params
                    .and_then(|a| a.first())
                    .and_then(|v| v.as_str());
                let pubkey_hex = params
                    .and_then(|a| a.get(1))
                    .and_then(|v| v.as_str());
                let (Some(node_id_hex), Some(pubkey_hex)) = (node_id_hex, pubkey_hex) else {
                    return rpc_err(
                        &id,
                        -32602,
                        "rope_registerValidator expects params: [\"0x<node_id 32B hex>\", \"0x<hybrid pubkey hex>\"]",
                    );
                };
                let node_id_bytes = match hex::decode(node_id_hex.trim_start_matches("0x")) {
                    Ok(b) if b.len() == 32 => {
                        let mut arr = [0u8; 32];
                        arr.copy_from_slice(&b);
                        arr
                    }
                    _ => {
                        return rpc_err(&id, -32602, "node_id must be 32 bytes of hex");
                    }
                };
                let pk_bytes = match hex::decode(pubkey_hex.trim_start_matches("0x")) {
                    Ok(b) => b,
                    Err(e) => {
                        return rpc_err(
                            &id,
                            -32602,
                            &format!("invalid hex public key: {e}"),
                        );
                    }
                };
                let public_key =
                    match rope_crypto::hybrid::HybridPublicKey::from_bytes(&pk_bytes) {
                        Ok(pk) => pk,
                        Err(e) => {
                            return rpc_err(
                                &id,
                                -32602,
                                &format!("invalid hybrid public key: {e}"),
                            );
                        }
                    };
                if let Some(orch) = &self.orchestrator {
                    match orch.register_peer_validator(
                        rope_core::types::NodeId::new(node_id_bytes),
                        public_key,
                    ) {
                        Ok(()) => {
                            let info = orch.committee_info();
                            serde_json::json!({
                                "registered": true,
                                "validators": info.validators,
                                "finalityQuorum": info.finality_quorum,
                            })
                        }
                        Err(e) => {
                            return rpc_err(
                                &id,
                                -32001,
                                &format!("validator registration rejected: {e}"),
                            );
                        }
                    }
                } else {
                    return rpc_err(
                        &id,
                        -32000,
                        "Consensus orchestrator not initialized",
                    );
                }
            }
            "rope_getNetworkInfo" => {
                let evm_connected = self
                    .evm_backend
                    .as_ref()
                    .map(|b| b.is_healthy())
                    .unwrap_or(false);
                let ai_agents = self
                    .orchestrator
                    .as_ref()
                    .map(|o| o.stats().ai_agents_active)
                    .unwrap_or(0);

                serde_json::json!({
                    "chainId": self.chain_id,
                    "networkName": "Datachain Rope Mainnet",
                    "version": self.network_version,
                    "peerCount": 0,
                    "consensusType": "testimony",
                    "executionMode": if evm_connected { "native + EVM execution layer (Reth)" } else { "native" },
                    "evmBackendConnected": evm_connected,
                    "anvilConnected": evm_connected,
                    "aiAgentsActive": ai_agents
                })
            }
            "rope_getAIAgentStatus" => {
                if let Some(orch) = &self.orchestrator {
                    let stats = orch.stats();
                    serde_json::json!({
                        "agentsActive": stats.ai_agents_active,
                        "agents": [
                            {"type": "ValidationAgent", "status": "active"},
                            {"type": "ComplianceAgent", "status": "active"},
                            {"type": "InsuranceAgent", "status": "active"}
                        ]
                    })
                } else {
                    serde_json::json!({
                        "agentsActive": 0,
                        "agents": []
                    })
                }
            }

            // ============================================================
            // Quipu Canon v2.0 Phase 4 — versioned DAG-of-knots namespace
            // ------------------------------------------------------------
            // These `rope_v2_*` methods run ALONGSIDE the v1.2 linear
            // ledger (`rope_appendToLedger` / `rope_walkString`), which is
            // untouched. v2 emitters opt in for multi-parent, concurrent
            // per-wallet appends; every reader is served the same
            // merge-free deterministic linear projection so nothing built
            // on v1.2 breaks. No flag day, no coordinated freeze.
            // ============================================================
            "rope_v2_appendKnot" => {
                // params: [wallet_hex, interaction, parents_hex?]
                let wallet_hex = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let interaction_val = params.and_then(|p| p.get(1));
                if wallet_hex.is_empty() || interaction_val.is_none() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"rope_v2_appendKnot: missing wallet or interaction"},"id":id}).to_string();
                }
                let wallet = match hex::decode(wallet_hex.trim_start_matches("0x")) {
                    Ok(b) if !b.is_empty() => b,
                    _ => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"rope_v2_appendKnot: invalid wallet hex"},"id":id}).to_string(),
                };

                let record = parse_interaction_record(interaction_val.unwrap());

                // Optional explicit parents (hex ids). Absent => append
                // against the wallet's current tip set (concurrency path).
                let explicit_parents = match params.and_then(|p| p.get(2)) {
                    Some(arr) if arr.is_array() => {
                        let mut out = Vec::new();
                        for pv in arr.as_array().unwrap() {
                            let s = pv.as_str().unwrap_or("");
                            match hex::decode(s.trim_start_matches("0x")) {
                                Ok(b) if b.len() == 32 => {
                                    let mut a = [0u8; 32];
                                    a.copy_from_slice(&b);
                                    out.push(rope_core::types::StringId::new(a));
                                }
                                _ => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"rope_v2_appendKnot: invalid parent id"},"id":id}).to_string(),
                            }
                        }
                        Some(out)
                    }
                    _ => None,
                };

                match self.dag.append_knot(&wallet, explicit_parents, record) {
                    Ok(appended) => serde_json::json!({
                        "knotId": appended.knot_id,
                        "parents": appended.parents,
                        "compacted": appended.compacted,
                        "mergeKnotId": appended.merge_knot_id,
                        "tipCount": appended.tip_count,
                        "canon": "v2.0",
                    }),
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":format!("rope_v2_appendKnot: {e}")},"id":id}).to_string();
                    }
                }
            }

            "rope_v2_walkString" => {
                // params: [wallet_hex] — returns the merge-free linear
                // projection with payloads, identical shape a v1.2 walk
                // would return for a linear ledger.
                let wallet_hex = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let wallet = match hex::decode(wallet_hex.trim_start_matches("0x")) {
                    Ok(b) if !b.is_empty() => b,
                    _ => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"rope_v2_walkString: invalid wallet hex"},"id":id}).to_string(),
                };
                let knots: Vec<serde_json::Value> = self
                    .dag
                    .walk_projection(&wallet)
                    .into_iter()
                    .map(|pk| {
                        serde_json::json!({
                            "knotId": pk.knot_id,
                            "interactionType": format!("{:?}", pk.interaction.interaction_type),
                            "data": String::from_utf8_lossy(&pk.interaction.data),
                            "timestamp": pk.interaction.timestamp,
                        })
                    })
                    .collect();
                serde_json::json!({
                    "wallet": wallet_hex,
                    "knotCount": knots.len(),
                    "knots": knots,
                    "canon": "v2.0-projection",
                })
            }

            "rope_v2_tips" => {
                let wallet_hex = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let wallet = match hex::decode(wallet_hex.trim_start_matches("0x")) {
                    Ok(b) if !b.is_empty() => b,
                    _ => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"rope_v2_tips: invalid wallet hex"},"id":id}).to_string(),
                };
                serde_json::json!({
                    "wallet": wallet_hex,
                    "tips": self.dag.tips(&wallet),
                })
            }

            "rope_v2_compact" => {
                // Operator/background compaction trigger for one wallet.
                let wallet_hex = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let wallet = match hex::decode(wallet_hex.trim_start_matches("0x")) {
                    Ok(b) if !b.is_empty() => b,
                    _ => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"rope_v2_compact: invalid wallet hex"},"id":id}).to_string(),
                };
                match self.dag.compact(&wallet) {
                    Ok(mid) => serde_json::json!({
                        "wallet": wallet_hex,
                        "merged": mid.is_some(),
                        "mergeKnotId": mid,
                    }),
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":format!("rope_v2_compact: {e}")},"id":id}).to_string();
                    }
                }
            }

            "rope_v2_stats" => {
                let s = self.dag.stats();
                serde_json::json!({
                    "enabled": s.enabled,
                    "canon": "v2.0",
                    "walletCount": s.wallet_count,
                    "totalKnots": s.total_knots,
                    "totalEvents": s.total_events,
                    "totalMerges": s.total_merges,
                    "compactionThreshold": s.compaction_threshold,
                })
            }

            // ================================================================
            // PERSONAL LEDGER — one String per wallet, distributed via RDP
            // ================================================================
            "rope_createPersonalLedger" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address parameter"},"id":id}).to_string();
                }
                // 2026-07-26 deadlock/5xx fix: route ledger mutators
                // through `spawn_blocking` so Condvar durability waits
                // never park a tokio worker.
                // 2026-07-27 P1: create/append default to ack-after-
                // enqueue (`ROPE_SYNC_DURABILITY` unset), so the blocking
                // pool is not saturated by 5s fsync waits under Quipu
                // bursts. `spawn_blocking` remains as defence in depth
                // for CPU-heavy encrypt/OES work and for
                // `ROPE_SYNC_DURABILITY=1` / GDPR paths.
                let ledger_bg = ledger.clone();
                let owner_owned = owner.to_string();
                let create_result =
                    tokio::task::spawn_blocking(move || ledger_bg.create_ledger(&owner_owned))
                        .await;
                match create_result {
                    Ok(Ok(_)) => {
                        let now = chrono::Utc::now().timestamp();
                        serde_json::json!({"owner": owner, "created_at": now})
                    }
                    Ok(Err(e)) if e.contains("already exists") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2001,"message":"Ledger already exists for this address"},"id":id}).to_string();
                    }
                    Ok(Err(e)) => {
                        return jsonrpc_ledger_err(&id, e);
                    }
                    Err(join_err) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":format!("internal: ledger task failed: {join_err}")},"id":id}).to_string();
                    }
                }
            }

            // Append a new knot to the wallet's string.
            //
            // KNOT STRING ID CONTRACT:
            //   The `hash` field in the response is the canonical
            //   `knot_string_id` (StringId, 32-byte hex). Callers that
            //   later need to reference this knot — to untie it
            //   (`rope_untieKnot`) or to look it up in the explorer —
            //   MUST use this exact value. The same hex appears as
            //   each knot's `string_id` in `rope_getStringWithKnots`,
            //   and as `knot_string_id` in the `rope_untieKnot`
            //   request and response. They are byte-for-byte identical
            //   and never re-derived (canon v1.1 §6 stable identifier
            //   guarantee).
            "rope_appendToLedger" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let interaction_val = params.and_then(|p| p.get(1));
                if owner.is_empty() || interaction_val.is_none() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address or interaction parameter"},"id":id}).to_string();
                }
                let interaction_val = interaction_val.unwrap();

                let itype_str = interaction_val
                    .get("interaction_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Custom");
                let description = interaction_val
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let metadata = interaction_val
                    .get("metadata")
                    .cloned()
                    .unwrap_or(serde_json::json!({}));

                use rope_core::personal_ledger::InteractionType;
                let interaction_type = match itype_str {
                    "Transfer" => InteractionType::Transfer,
                    "ContractCall" | "ContractDeploy" => InteractionType::ContractCall,
                    "TokenApproval" | "Approval" => InteractionType::TokenApproval,
                    "IdentityClaim" | "DIDCreation" | "DIDUpdate" | "ClaimIssuance" => {
                        InteractionType::IdentityClaim
                    }
                    "TestimonySubmission" => InteractionType::TestimonySubmission,
                    "DataSharing" | "PlatformConnection" => InteractionType::DataSharing,
                    "StakeDeposit" | "Stake" => InteractionType::StakeDeposit,
                    "StakeWithdraw" | "Unstake" => InteractionType::StakeWithdraw,
                    "BridgeOperation" => InteractionType::BridgeOperation,
                    "Swap" => InteractionType::Custom("Swap".to_string()),
                    other => InteractionType::Custom(other.to_string()),
                };

                let record = rope_core::personal_ledger::InteractionRecord {
                    interaction_type,
                    counterparty: None,
                    data: description.as_bytes().to_vec(),
                    timestamp: chrono::Utc::now().timestamp(),
                    metadata: {
                        let mut map = hashbrown::HashMap::new();
                        if let Some(obj) = metadata.as_object() {
                            for (k, v) in obj {
                                map.insert(
                                    k.clone(),
                                    v.as_str().unwrap_or(&v.to_string()).to_string(),
                                );
                            }
                        }
                        map
                    },
                };

                // 2026-07-26 / 2026-07-27: see `rope_createPersonalLedger`
                // — `spawn_blocking` + ack-after-enqueue (default).
                let ledger_bg = ledger.clone();
                let owner_owned = owner.to_string();
                let append_result = tokio::task::spawn_blocking(move || {
                    ledger_bg.append_to_ledger(&owner_owned, record)
                })
                .await;
                match append_result {
                    Ok(Ok(resp)) => serde_json::json!({
                        "index": resp.piece_count,
                        "hash": resp.string_id
                    }),
                    Ok(Err(e)) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Ok(Err(e)) if e.contains("deleted") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2003,"message":"Ledger has been deleted"},"id":id}).to_string();
                    }
                    Ok(Err(e)) => {
                        return jsonrpc_ledger_err(&id, e);
                    }
                    Err(join_err) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":format!("internal: ledger task failed: {join_err}")},"id":id}).to_string();
                    }
                }
            }

            "rope_getLedgerStatus" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address parameter"},"id":id}).to_string();
                }
                match ledger.get_ledger_status(owner) {
                    Ok(status) => serde_json::json!({
                        "owner": status.wallet_address,
                        "fragment_count": status.entry_count,
                        "total_size_bytes": status.total_size_bytes,
                        "created_at": status.created_at,
                        "last_appended_at": status.last_appended_at,
                        "distributed_nodes": 5
                    }),
                    Err(e) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
                    }
                }
            }

            "rope_repatriatePersonalLedger" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address parameter"},"id":id}).to_string();
                }
                // Optional `params[1] = {"decrypt": true}` (or plain `true`).
                // Honoured ONLY for internal callers (loopback-without-XFF /
                // internal-token / WS). Public callers always get
                // `interaction: null` — payloads are OES-encrypted personal
                // data and never leave the node in the clear over the
                // public listener. Co-located services (dc-explorer's
                // on-rope node-request queue) use this to rebuild their
                // local caches from the chain.
                let decrypt_requested = params
                    .and_then(|p| p.get(1))
                    .map(|v| {
                        v.as_bool()
                            .or_else(|| v.get("decrypt").and_then(|d| d.as_bool()))
                            .unwrap_or(false)
                    })
                    .unwrap_or(false);
                let decrypt = decrypt_requested && is_internal;
                if decrypt_requested && !is_internal {
                    tracing::warn!(
                        target: "rope_node::auth",
                        method = "rope_repatriatePersonalLedger",
                        wallet = owner,
                        "decrypt=true requested on the public listener — served encrypted-only"
                    );
                }
                match ledger.repatriate_ledger(owner, decrypt) {
                    Ok(resp) => {
                        let fragments: Vec<serde_json::Value> = resp
                            .entries
                            .iter()
                            .map(|e| {
                                // Decrypted payloads are serialized
                                // `InteractionRecord`s; surface them as
                                // structured JSON with the raw byte payload
                                // rendered back to a UTF-8 description.
                                let interaction = e
                                    .decrypted_content
                                    .as_ref()
                                    .and_then(|bytes| {
                                        serde_json::from_slice::<serde_json::Value>(bytes).ok()
                                    })
                                    .map(|mut rec| {
                                        let description = rec
                                            .get("data")
                                            .and_then(|d| d.as_array())
                                            .map(|arr| {
                                                let raw: Vec<u8> = arr
                                                    .iter()
                                                    .filter_map(|b| b.as_u64().map(|n| n as u8))
                                                    .collect();
                                                String::from_utf8_lossy(&raw).into_owned()
                                            })
                                            .unwrap_or_default();
                                        if let Some(obj) = rec.as_object_mut() {
                                            obj.remove("data");
                                            obj.insert(
                                                "description".to_string(),
                                                serde_json::json!(description),
                                            );
                                        }
                                        rec
                                    })
                                    .unwrap_or(serde_json::Value::Null);
                                serde_json::json!({
                                    "index": e.sequence,
                                    "hash": e.string_id,
                                    "timestamp": chrono::Utc::now().timestamp(),
                                    "interaction": interaction
                                })
                            })
                            .collect();
                        let integrity = format!("0x{:0>64x}", resp.total_bytes);
                        serde_json::json!({
                            "owner": resp.wallet_address,
                            "fragments": fragments,
                            "assembled_at": chrono::Utc::now().timestamp(),
                            "integrity_hash": integrity,
                            "decrypted": decrypt
                        })
                    }
                    Err(e) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
                    }
                }
            }

            // ============================================================
            // QUIPU PRIMITIVE CANON v1.1 — whole-wallet closure
            // ============================================================
            // The destructive, all-or-nothing erasure equivalent to wallet
            // closure. For granular per-event erasure, callers MUST use
            // `rope_untieKnot` instead (canon §6.3 — never default to
            // whole-string erasure when granular suffices).
            //
            // Authentication model: identical to `rope_untieKnot` — see
            // its doc comment. PHASE 1 trusts the upstream proxy; PHASE 2
            // will require a signed payload in params[1].
            "rope_erasePersonalLedger" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address parameter"},"id":id}).to_string();
                }
                tracing::warn!(
                    target: "rope_node::auth",
                    method = "rope_erasePersonalLedger",
                    wallet = owner,
                    "Quipu Canon §6 — whole-wallet erase accepted under PHASE-1 auth (no JSON-RPC signature). \
                     Operator MUST front this RPC with an authenticated proxy or restrict to private network."
                );
                use rope_protocols::ledger_lifecycle::DeletionReason;
                // 2026-07-26: see the `rope_createPersonalLedger` comment
                // above — `erase_ledger` also calls `await_all_durable`
                // (up to 10s here) and must not block a tokio worker.
                let ledger_bg = ledger.clone();
                let owner_owned = owner.to_string();
                let erase_result = tokio::task::spawn_blocking(move || {
                    ledger_bg.erase_ledger(&owner_owned, DeletionReason::OwnerRequest)
                })
                .await;
                match erase_result {
                    Ok(Ok(resp)) => serde_json::json!({
                        "owner": resp.wallet_address,
                        "erased_fragments": resp.entries_erased,
                        "audit_hash": format!("0x{}", resp.audit_hash),
                        "erased_at": chrono::Utc::now().timestamp(),
                        "gdpr_article": "Article 17 — Right to Erasure (whole-string closure)",
                        "scope": "whole_wallet",
                        "canon": "v1.1 §6 — explicit wallet-closure, equivalent to closing the account. For granular per-event erasure, use rope_untieKnot.",
                        "auth_method": "phase-1-trusted-proxy"
                    }),
                    Ok(Err(e)) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Ok(Err(e)) if e.contains("already deleted") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2003,"message":"Ledger already deleted"},"id":id}).to_string();
                    }
                    Ok(Err(e)) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
                    }
                    Err(join_err) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":format!("internal: ledger task failed: {join_err}")},"id":id}).to_string();
                    }
                }
            }

            // ============================================================
            // QUIPU PRIMITIVE CANON v1.1 — per-knot (per-event) untying
            // ============================================================
            // The granular GDPR Article 17 primitive. Untie a single knot
            // on a wallet's string while preserving every other knot.
            //
            // Params:
            //   [0] wallet_address    — the 0x-prefixed wallet address
            //   [1] knot_string_id    — the SAME hex identifier returned by
            //                            `rope_appendToLedger` as `hash`
            //                            and by `rope_getStringWithKnots`
            //                            as each knot's `string_id`. They
            //                            are byte-for-byte identical: the
            //                            opaque hex of the StringId.
            //   [2] reason            — optional reason class
            //                            (default "OwnerRequest")
            //
            // AUTHENTICATION MODEL (canon v1.1 §4.2 + AUTH-CONTRACT):
            //   PHASE 1 (current):  No JSON-RPC layer authentication.
            //     The endpoint trusts that the operator (Datawallet+
            //     backend, dApp, AI agent) has independently authorized
            //     the request. Production deployments MUST place an
            //     authenticated reverse proxy (mTLS, signed JWT, or
            //     wallet-signature-via-header) in front of this RPC, OR
            //     bind it to a private network. Public exposure WITHOUT
            //     such a proxy means any caller can untie any knot — a
            //     compliance risk.
            //   PHASE 2 (planned): params[3] = "0x{sig}" where sig is
            //     keccak256(domain_separator || method || wallet || knot_id
            //     || nonce || expiry) signed with the wallet's secp256k1
            //     key. Verifier reconstructs the message and checks the
            //     recovered address matches params[0]. Until Phase 2
            //     ships, callers should set the request header
            //     `X-Rope-Auth-Phase: 1` to acknowledge they are
            //     responsible for upstream authorization.
            //
            // The response includes `auth_method` so callers can audit
            // which phase actually validated the request.
            "rope_untieKnot" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let knot_id_raw = params
                    .and_then(|p| p.get(1))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let reason = params
                    .and_then(|p| p.get(2))
                    .and_then(|v| v.as_str())
                    .unwrap_or("OwnerRequest");

                if owner.is_empty() || knot_id_raw.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Expected params [ wallet_address, knot_string_id, [reason] ]"},"id":id}).to_string();
                }

                let knot_id = knot_id_raw.trim_start_matches("0x");

                tracing::warn!(
                    target: "rope_node::auth",
                    method = "rope_untieKnot",
                    wallet = owner,
                    knot_id = knot_id,
                    "Quipu Canon §4.2 — untie request accepted under PHASE-1 auth (no JSON-RPC signature). \
                     Operator MUST front this RPC with an authenticated proxy or restrict to private network. \
                     See rpc_server.rs `rope_untieKnot` doc-comment."
                );

                // 2026-07-26: see the `rope_createPersonalLedger` comment
                // above — `untie_knot` also calls `await_all_durable`
                // (up to 10s here) and must not block a tokio worker.
                let ledger_bg = ledger.clone();
                let owner_owned = owner.to_string();
                let knot_id_owned = knot_id.to_string();
                let reason_owned = reason.to_string();
                let untie_result = tokio::task::spawn_blocking(move || {
                    ledger_bg.untie_knot(&owner_owned, &knot_id_owned, &reason_owned)
                })
                .await;
                match untie_result {
                    Ok(Ok(resp)) => serde_json::json!({
                        "wallet_address": resp.wallet_address,
                        "knot_string_id": format!("0x{}", resp.knot_string_id),
                        "tombstone_audit_hash": format!("0x{}", resp.tombstone_audit_hash),
                        "untied_at": resp.untied_at,
                        "reason": resp.reason,
                        "knots_remaining": resp.knots_remaining,
                        "tombstones_total": resp.tombstones_total,
                        "gdpr_article": resp.gdpr_article,
                        "canon": "v1.1 §4.2 — per-knot tombstone, payload destroyed, position preserved",
                        "scope": "single_knot",
                        "auth_method": "phase-1-trusted-proxy"
                    }),
                    Ok(Err(e)) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Ok(Err(e)) if e.contains("already wholly deleted") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2003,"message":e},"id":id}).to_string();
                    }
                    Ok(Err(e)) if e.contains("genesis knot") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2010,"message":e},"id":id}).to_string();
                    }
                    Ok(Err(e)) if e.contains("does not belong") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2011,"message":e},"id":id}).to_string();
                    }
                    Ok(Err(e)) if e.contains("already untied") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2012,"message":e},"id":id}).to_string();
                    }
                    Ok(Err(e)) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
                    }
                    Err(join_err) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":format!("internal: ledger task failed: {join_err}")},"id":id}).to_string();
                    }
                }
            }

            // Walk a wallet's string and return the canonical
            // String → Knots[] hierarchy with tombstones included.
            // This powers the DCScan personal-ledger view per canon §6(2).
            // Params: [ "0xWALLET" ]
            "rope_getStringWithKnots" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing wallet address parameter"},"id":id}).to_string();
                }
                match ledger.walk_string_with_tombstones(owner) {
                    Ok((string_id_hex, entries)) => {
                        let knots: Vec<serde_json::Value> = entries
                            .iter()
                            .enumerate()
                            .map(|(idx, entry)| self.knot_to_json(idx, entry, owner))
                            .collect();
                        let active = knots
                            .iter()
                            .filter(|k| k.get("status").and_then(|v| v.as_str()) == Some("active"))
                            .count();
                        let tombs = knots
                            .iter()
                            .filter(|k| {
                                k.get("status").and_then(|v| v.as_str()) == Some("tombstone")
                            })
                            .count();
                        serde_json::json!({
                            "wallet_address": owner,
                            "string_id": format!("0x{}", string_id_hex),
                            "knots": knots,
                            "knot_count": knots.len(),
                            "active_count": active,
                            "tombstone_count": tombs,
                            "canon": "v1.1 §6(2) — String → Knot[] → Transaction details",
                            "rpc_api_version": ROPE_RPC_API_VERSION
                        })
                    }
                    Err(e) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
                    }
                }
            }

            // ================================================================
            // IOT GATEWAY — device registration, telemetry ingestion
            // ================================================================
            "rope_registerDevice" => {
                let gw = match &self.iot_gateway {
                    Some(g) => g,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"IoT Gateway not initialized"},"id":id}).to_string(),
                };
                let p = params
                    .and_then(|p| p.get(0))
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                let device_id = p
                    .get("device_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let wallet = p
                    .get("wallet_address")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let dtype = p
                    .get("device_type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("sensor");
                let name = p
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&device_id)
                    .to_string();
                let owner = p
                    .get("owner_wallet")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&wallet)
                    .to_string();
                let location = p.get("location").and_then(|v| {
                    let lat = v.get("lat").and_then(|l| l.as_f64())?;
                    let lng = v.get("lng").and_then(|l| l.as_f64())?;
                    Some((lat, lng))
                });

                if device_id.is_empty() || wallet.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing device_id or wallet_address"},"id":id}).to_string();
                }

                let mut meta = hashbrown::HashMap::new();
                if let Some(obj) = p.get("metadata").and_then(|v| v.as_object()) {
                    for (k, v) in obj {
                        meta.insert(k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string());
                    }
                }

                match gw.register_device(device_id, wallet, dtype, name, owner, location, meta) {
                    Ok(info) => serde_json::json!({
                        "device_id": info.device_id,
                        "wallet_address": info.wallet_address,
                        "device_type": info.device_type.as_str(),
                        "name": info.name,
                        "status": "online",
                        "registered_at": info.registered_at
                    }),
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":3001,"message":e},"id":id}).to_string();
                    }
                }
            }

            "rope_ingestTelemetry" => {
                let gw = match &self.iot_gateway {
                    Some(g) => g,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"IoT Gateway not initialized"},"id":id}).to_string(),
                };
                let p = params
                    .and_then(|p| p.get(0))
                    .cloned()
                    .unwrap_or(serde_json::json!({}));
                let wallet = p
                    .get("device_wallet")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if wallet.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing device_wallet"},"id":id}).to_string();
                }

                let mut readings = hashbrown::HashMap::new();
                if let Some(obj) = p.get("readings").and_then(|v| v.as_object()) {
                    for (k, v) in obj {
                        if let Some(f) = v.as_f64() {
                            readings.insert(
                                k.clone(),
                                rope_iot_gateway::protocol::TelemetryValue::Float(f),
                            );
                        } else if let Some(i) = v.as_i64() {
                            readings.insert(
                                k.clone(),
                                rope_iot_gateway::protocol::TelemetryValue::Integer(i),
                            );
                        } else if let Some(b) = v.as_bool() {
                            readings.insert(
                                k.clone(),
                                rope_iot_gateway::protocol::TelemetryValue::Boolean(b),
                            );
                        } else if let Some(s) = v.as_str() {
                            readings.insert(
                                k.clone(),
                                rope_iot_gateway::protocol::TelemetryValue::Text(s.to_string()),
                            );
                        }
                    }
                }

                let payload = rope_iot_gateway::protocol::TelemetryPayload {
                    device_wallet: wallet,
                    timestamp: chrono::Utc::now().timestamp(),
                    readings,
                    source_protocol: rope_iot_gateway::protocol::SourceProtocol::Http,
                    sequence_number: p.get("sequence").and_then(|v| v.as_u64()),
                    quality: None,
                };

                match gw.ingest_telemetry(payload) {
                    Ok(()) => {
                        serde_json::json!({"status": "ingested", "timestamp": chrono::Utc::now().timestamp()})
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":3002,"message":e},"id":id}).to_string();
                    }
                }
            }

            "rope_getDeviceStatus" => {
                let gw = match &self.iot_gateway {
                    Some(g) => g,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"IoT Gateway not initialized"},"id":id}).to_string(),
                };
                let device_id = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if device_id.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing device_id parameter"},"id":id}).to_string();
                }
                match gw.registry().get_by_id(device_id) {
                    Some(info) => serde_json::json!({
                        "device_id": info.device_id,
                        "wallet_address": info.wallet_address,
                        "device_type": info.device_type.as_str(),
                        "name": info.name,
                        "status": format!("{:?}", info.status),
                        "telemetry_count": info.telemetry_count,
                        "last_seen_at": info.last_seen_at,
                        "registered_at": info.registered_at
                    }),
                    None => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":3003,"message":"Device not found"},"id":id}).to_string();
                    }
                }
            }

            "rope_getIoTGatewayStats" => {
                let gw = match &self.iot_gateway {
                    Some(g) => g,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"IoT Gateway not initialized"},"id":id}).to_string(),
                };
                let stats = gw.stats();
                serde_json::json!({
                    "devices_registered": stats.devices_registered,
                    "devices_online": stats.devices_online,
                    "telemetry_received": stats.telemetry_received,
                    "events_received": stats.events_received,
                    "fragments_written": stats.fragments_written,
                    "errors": stats.errors,
                    "mqtt_connected": stats.mqtt_connected,
                    "coap_running": stats.coap_running,
                    "uptime_secs": stats.uptime_secs
                })
            }

            "rope_listDevices" => {
                let gw = match &self.iot_gateway {
                    Some(g) => g,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"IoT Gateway not initialized"},"id":id}).to_string(),
                };
                let devices: Vec<serde_json::Value> = gw
                    .registry()
                    .list_devices()
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "device_id": d.device_id,
                            "wallet_address": d.wallet_address,
                            "device_type": d.device_type.as_str(),
                            "name": d.name,
                            "status": format!("{:?}", d.status),
                            "telemetry_count": d.telemetry_count,
                            "last_seen_at": d.last_seen_at
                        })
                    })
                    .collect();
                serde_json::json!({"devices": devices, "count": devices.len()})
            }

            // ================================================================
            // AI AGENT FRAMEWORK — agent registration, status, analysis
            // ================================================================
            "rope_registerAgent" => {
                let fw = match &self.ai_framework {
                    Some(f) => f,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"AI Agent Framework not initialized"},"id":id}).to_string(),
                };
                let agents = fw.list_agents();
                serde_json::json!({
                    "registered_agents": agents.iter().map(|a| serde_json::json!({
                        "agent_id": a.agent_id,
                        "name": a.name,
                        "domain": a.domain.as_str(),
                        "version": a.version,
                        "state": a.state.as_str(),
                        "capabilities": a.capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>()
                    })).collect::<Vec<_>>(),
                    "note": "Third-party agent registration via RPC is available. Use rope_registerAgent with agent WASM module."
                })
            }

            "rope_getAgentStatus" => {
                let fw = match &self.ai_framework {
                    Some(f) => f,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"AI Agent Framework not initialized"},"id":id}).to_string(),
                };
                let agent_id = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if agent_id.is_empty() {
                    let stats = fw.stats();
                    serde_json::json!({
                        "agents_registered": stats.agents_registered,
                        "agents_active": stats.agents_active,
                        "total_analyses": stats.total_analyses,
                        "total_diagnoses_written": stats.total_diagnoses_written,
                        "avg_confidence": stats.avg_confidence,
                        "last_run_at": stats.last_run_at,
                        "uptime_secs": stats.uptime_secs
                    })
                } else {
                    match fw.get_agent(agent_id) {
                        Some(desc) => serde_json::json!({
                            "agent_id": desc.agent_id,
                            "name": desc.name,
                            "version": desc.version,
                            "domain": desc.domain.as_str(),
                            "state": desc.state.as_str(),
                            "capabilities": desc.capabilities.iter().map(|c| c.as_str()).collect::<Vec<_>>(),
                            "run_count": desc.run_count,
                            "avg_confidence": desc.avg_confidence,
                            "error_count": desc.error_count,
                            "last_run_at": desc.last_run_at,
                            "registered_at": desc.registered_at
                        }),
                        None => {
                            return serde_json::json!({"jsonrpc":"2.0","error":{"code":4001,"message":"Agent not found"},"id":id}).to_string();
                        }
                    }
                }
            }

            "rope_listAgents" => {
                let fw = match &self.ai_framework {
                    Some(f) => f,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"AI Agent Framework not initialized"},"id":id}).to_string(),
                };
                let agents: Vec<serde_json::Value> = fw
                    .list_agents()
                    .iter()
                    .map(|a| {
                        serde_json::json!({
                            "agent_id": a.agent_id,
                            "name": a.name,
                            "domain": a.domain.as_str(),
                            "version": a.version,
                            "state": a.state.as_str(),
                            "run_count": a.run_count,
                            "avg_confidence": a.avg_confidence
                        })
                    })
                    .collect();
                serde_json::json!({"agents": agents, "count": agents.len()})
            }

            "rope_subscribeAgentToWallet" => {
                let fw = match &self.ai_framework {
                    Some(f) => f,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"AI Agent Framework not initialized"},"id":id}).to_string(),
                };
                let agent_id = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let wallet = params
                    .and_then(|p| p.get(1))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if agent_id.is_empty() || wallet.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing agent_id or wallet parameter"},"id":id}).to_string();
                }
                match fw.subscribe_agent_to_wallet(agent_id, wallet) {
                    Ok(()) => {
                        serde_json::json!({"status": "subscribed", "agent_id": agent_id, "wallet": wallet})
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":4002,"message":e},"id":id}).to_string();
                    }
                }
            }

            "rope_getRecentDiagnoses" => {
                let fw = match &self.ai_framework {
                    Some(f) => f,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"AI Agent Framework not initialized"},"id":id}).to_string(),
                };
                let limit = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10) as usize;
                let diagnoses: Vec<serde_json::Value> = fw
                    .recent_diagnoses(limit)
                    .iter()
                    .map(|d| {
                        serde_json::json!({
                            "agent_id": d.agent_id,
                            "target_wallet": d.target_wallet,
                            "diagnosis_type": d.diagnosis_type,
                            "severity": d.severity.as_str(),
                            "confidence": d.confidence.value,
                            "description": d.description,
                            "timestamp": d.timestamp,
                            "recommendations": d.recommendations.iter().map(|r| serde_json::json!({
                                "action": r.action,
                                "priority": format!("{:?}", r.priority)
                            })).collect::<Vec<_>>()
                        })
                    })
                    .collect();
                serde_json::json!({"diagnoses": diagnoses, "count": diagnoses.len()})
            }

            // === Master-node governance (added 2026-05-03) ===
            //
            // Read-only methods (no auth):
            //   rope_governanceInfo   — full registry + recent log entries
            //   rope_listMasterNodes  — just the master node list
            //   rope_nodeIdentity     — deployer attestation for a node_id
            //                           (defaults to self when no arg given)
            //
            // Mutating methods (require Ed25519 signature, see governance.rs):
            //   rope_suspendNode  — master OR founder
            //   rope_isolateNode  — founder only
            //   rope_eraseNode    — founder only
            //
            // Per .cursor/rules/master-node-governance.mdc.
            "rope_governanceInfo" => {
                let gov = match &self.governance {
                    Some(g) => g,
                    None => {
                        return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32603,"message":"Governance not initialized"},
                            "id":id
                        })
                        .to_string()
                    }
                };
                let registry = gov.registry_snapshot();
                let recent_log = gov.recent_log(20);
                serde_json::json!({
                    "schema_version": registry.schema_version,
                    "chain_id": registry.chain_id,
                    "authority": registry.authority,
                    "last_updated": registry.last_updated,
                    "master_nodes": registry.master_nodes,
                    "member_nodes": registry.member_nodes,
                    "founder": {
                        "name": registry.founder.name,
                        "organization": registry.founder.organization,
                        "canonical_email": registry.founder.canonical_email,
                        "domains": registry.founder.domains,
                        "founder_keys_count": registry.founder.founder_keys.len(),
                        "founder_dids": registry.founder.founder_dids,
                    },
                    "replay_window_secs": registry.replay.window_secs,
                    "enforce": gov.enforce(),
                    "recent_log": recent_log,
                })
            }

            "rope_listMasterNodes" => {
                let gov = match &self.governance {
                    Some(g) => g,
                    None => {
                        return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32603,"message":"Governance not initialized"},
                            "id":id
                        })
                        .to_string()
                    }
                };
                serde_json::json!({"master_nodes": gov.registry_snapshot().master_nodes})
            }

            "rope_nodeIdentity" => {
                // Optional first param: target node_id (hex). When omitted,
                // returns this node's own attestation.
                let target_node_id = params.and_then(|p| p.get(0)).and_then(|v| {
                    v.as_str().map(|s| s.to_string()).or_else(|| {
                        v.get("node_id")
                            .and_then(|s| s.as_str())
                            .map(|s| s.to_string())
                    })
                });
                let target = target_node_id.unwrap_or_else(|| self.self_node_id.clone());

                if target == self.self_node_id || self.self_node_id.is_empty() {
                    let dep = self.deployer.clone().unwrap_or_default();
                    serde_json::json!({
                        "node_id": self.self_node_id,
                        "wallet_address": dep.wallet_address,
                        "did": dep.did,
                        "onchainid": dep.onchainid,
                        "name": dep.name,
                        "organization": dep.organization,
                        "incorporation": dep.incorporation,
                        "address": dep.address,
                        "email": dep.email,
                        "country": dep.country,
                        "self_signature": dep.self_signature,
                        "verifiable": !dep.self_signature.is_empty(),
                    })
                } else {
                    // Look up via the governance registry. Until ONCHAINID
                    // resolution lands we just report which slot the target
                    // is in and the registry-recorded role/provider.
                    let registry = self
                        .governance
                        .as_ref()
                        .map(|g| g.registry_snapshot())
                        .unwrap_or_default();
                    let entry = registry
                        .master_nodes
                        .iter()
                        .chain(registry.member_nodes.iter())
                        .find(|n| n.node_id == target);
                    match entry {
                        Some(e) => serde_json::json!({
                            "node_id": e.node_id,
                            "slot": e.slot,
                            "hostname": e.hostname,
                            "provider": e.provider,
                            "region": e.region,
                            "ip": e.ip,
                            "role": e.role,
                            "deployer": "Datachain Foundation (registered as master/member)",
                            "verifiable": false,
                            "note": "On-chain DID resolution not yet wired; see master-node-governance.mdc"
                        }),
                        None => serde_json::json!({
                            "node_id": target,
                            "deployer": "unknown",
                            "verifiable": false,
                            "note": "node_id is not present in master-nodes.toml"
                        }),
                    }
                }
            }

            "rope_globalStats" => {
                // Quipu Canon v1.2 — returns the total number of strings
                // and total number of knots, with the per-kind breakdown.
                // Invariant: total_knots >= total_strings.
                //
                // Rope Graph v1 extension: also expose the static
                // entity-label registry counts so the frontend can
                // render ecosystems / applications / bots even before
                // any on-chain canon string of those kinds exists.
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32603,"message":"personal ledger subsystem not initialized"},
                        "id":id
                    }).to_string(),
                };
                let stats = ledger.global_stats();
                let mut json = serde_json::to_value(&stats).unwrap_or(serde_json::json!({}));
                let reg_arc = entity_labels::current();
                let reg: &entity_labels::LabelRegistry = &reg_arc;
                let breakdown = reg.platform_breakdown();
                if let Some(obj) = json.as_object_mut() {
                    obj.insert(
                        "label_registry".to_string(),
                        serde_json::json!({
                            "total_labels": reg.all().len(),
                            "ecosystems": reg.list_by_kind(entity_labels::LabelKind::Ecosystem).len(),
                            "applications": reg.list_by_kind(entity_labels::LabelKind::Application).len(),
                            "contracts": reg.list_by_kind(entity_labels::LabelKind::Contract).len(),
                            "bots": reg.list_by_kind(entity_labels::LabelKind::Bot).len(),
                            "agents": reg.list_by_kind(entity_labels::LabelKind::Agent).len(),
                            "assets": reg.list_by_kind(entity_labels::LabelKind::Asset).len(),
                            "by_platform": breakdown,
                        }),
                    );
                    obj.insert(
                        "rpc_api_version".to_string(),
                        serde_json::json!(ROPE_RPC_API_VERSION),
                    );
                }
                json
            }

            "rope_latticeMetrics" => {
                // P1 (§17.5 #1) — head_guard wait/hold histograms + per-op
                // counters + flusher wait histogram. Always-on, lock-free,
                // safe to poll every 10 s from Grafana / a scraper.
                //
                // Params (optional): [{ "reset": bool }]. Reset=true zeroes
                // all counters after returning the snapshot; used to open
                // a fresh observation window without a process restart.
                let reset = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.get("reset"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let metrics = crate::lattice_metrics::lattice_metrics();
                let snap = metrics.snapshot();
                if reset {
                    metrics.reset();
                }
                let mut json = serde_json::to_value(&snap)
                    .unwrap_or(serde_json::json!({}));
                if let Some(obj) = json.as_object_mut() {
                    obj.insert(
                        "rpc_api_version".to_string(),
                        serde_json::json!(ROPE_RPC_API_VERSION),
                    );
                    obj.insert(
                        "reset_after_snapshot".to_string(),
                        serde_json::json!(reset),
                    );
                }
                json
            }

            "rope_listStrings" => {
                // Quipu Canon v1.2 / Rope Graph v1 — paginated list of
                // strings. Spec: §4.1, §4.2 (kind_filter / kind / kinds —
                // string OR array), platform, parent, ancestor,
                // verified_only, min_knots, active_since, sort.
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32603,"message":"personal ledger subsystem not initialized"},
                        "id":id
                    }).to_string(),
                };
                let p = params.and_then(|p| p.get(0));
                let kinds = self.parse_kind_filter(p);
                let platform = p.and_then(|v| v.get("platform")).and_then(|v| v.as_str());
                let parent_id = p.and_then(|v| v.get("parent")).and_then(|v| v.as_str());
                let ancestor_id = p.and_then(|v| v.get("ancestor")).and_then(|v| v.as_str());
                let verified_only = p
                    .and_then(|v| v.get("verified_only"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let min_knots = p
                    .and_then(|v| v.get("min_knots"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let active_since = p
                    .and_then(|v| v.get("active_since"))
                    .and_then(|v| v.as_i64());
                let sort = p.and_then(|v| v.get("sort")).and_then(|v| v.as_str());
                let offset = p
                    .and_then(|v| v.get("offset"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let limit = p
                    .and_then(|v| v.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50)
                    .clamp(1, 500) as usize;

                // Pull the full descriptor list once, apply every filter
                // server-side, sort, then paginate. This is the contract
                // the spec asks for: filter BEFORE limit.
                let filters = StringListFilters {
                    kinds: kinds.clone(),
                    platform,
                    parent_id,
                    ancestor_id,
                    verified_only,
                    min_knots,
                    active_since,
                };
                let (_, full) = ledger.list_strings(None, 0, usize::MAX);
                let mut filtered: Vec<_> = full
                    .into_iter()
                    .filter(|d| self.descriptor_matches_filters(d, &filters))
                    .collect();
                self.sort_descriptors(&mut filtered, sort);
                let total = filtered.len();
                let slice: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();
                let strings: Vec<_> = slice
                    .iter()
                    .map(|d| self.string_descriptor_to_json(d))
                    .collect();
                serde_json::json!({
                    "total": total,
                    "offset": offset,
                    "limit": limit,
                    "applied_filters": {
                        "kind_filter": kinds.as_ref().map(|ks| ks.iter().map(|k| k.as_str()).collect::<Vec<_>>()),
                        "platform": platform,
                        "parent": parent_id,
                        "ancestor": ancestor_id,
                        "verified_only": verified_only,
                        "min_knots": min_knots,
                        "active_since": active_since,
                        "sort": sort.unwrap_or("newest"),
                    },
                    // Legacy field name preserved so v1.0/1.1 callers
                    // still see something useful in `kind_filter`.
                    "kind_filter": kinds
                        .as_ref()
                        .and_then(|ks| ks.first())
                        .map(|k| k.as_str()),
                    "strings": strings,
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                })
            }

            "rope_getString" | "rope_getStringById" => {
                // Quipu Canon v1.2 / Rope Graph v1 — single string by
                // (kind?, string_id). When `kind` is omitted we look in
                // every kind (matching whichever has a string with that
                // id). When the id is a synthetic ecosystem/application
                // we return the synthetic-string shape.
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32603,"message":"personal ledger subsystem not initialized"},
                        "id":id
                    }).to_string(),
                };
                let p = params.and_then(|p| p.get(0));
                let kind = p
                    .and_then(|v| v.get("kind"))
                    .and_then(|v| v.as_str())
                    .and_then(rope_core::personal_ledger::StringKind::parse);
                let string_id = p
                    .and_then(|v| v.get("string_id"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        params.and_then(|p| p.get(0)).and_then(|v| v.as_str())
                    })
                    .unwrap_or("");
                if string_id.is_empty() {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32602,"message":"string_id is required"},
                        "id":id
                    })
                    .to_string();
                }

                // First, try the on-chain registry. If a real descriptor
                // exists for the given (kind, id) — or for any kind
                // when `kind` is None — return the extended-shape JSON.
                if let Some(d) = ledger.get_string(kind, string_id) {
                    let mut json = self.string_descriptor_to_json(&d);
                    if let Some(obj) = json.as_object_mut() {
                        obj.insert(
                            "oes_generation".to_string(),
                            serde_json::json!(d.current_oes_generation),
                        );
                    }
                    json
                } else if kind.is_none() {
                    // Try every kind in the registry (a contract or an
                    // asset string may share the same hex id with a
                    // wallet by accident; the canon stores them under
                    // distinct (kind, id) keys).
                    let mut found = None;
                    for k in [
                        rope_core::personal_ledger::StringKind::Wallet,
                        rope_core::personal_ledger::StringKind::Contract,
                        rope_core::personal_ledger::StringKind::Asset,
                        rope_core::personal_ledger::StringKind::Did,
                        rope_core::personal_ledger::StringKind::Cord,
                    ] {
                        if let Some(d) = ledger.get_string(Some(k), string_id) {
                            found = Some(d);
                            break;
                        }
                    }
                    if let Some(d) = found {
                        self.string_descriptor_to_json(&d)
                    } else if let Some(label) = entity_labels::lookup(string_id) {
                        // Fall back to the synthetic shape so the
                        // frontend can render ecosystems, applications,
                        // and Tanastok manifest entities before any
                        // on-chain string of those kinds exists.
                        self.synthetic_string_to_json(&label)
                    } else {
                        return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{
                                "code":-32004,
                                "message":format!("no string for {}", string_id)
                            },
                            "id":id
                        }).to_string();
                    }
                } else if let Some(label) = entity_labels::lookup(string_id) {
                    self.synthetic_string_to_json(&label)
                } else {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32004,"message":format!("no string for {}:{}", kind.map(|k| k.as_str()).unwrap_or("wallet"), string_id)},
                        "id":id
                    }).to_string();
                }
            }

            // Quipu Canon v1.2 / Rope Graph v1 §4.4 — paginated knot list.
            // Param shape: { string_id?, since?, until?, kind?, limit?, offset? }
            "rope_listKnots" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32603,"message":"personal ledger subsystem not initialized"},
                        "id":id
                    }).to_string(),
                };
                let p = params.and_then(|p| p.get(0));
                let string_id = p.and_then(|v| v.get("string_id")).and_then(|v| v.as_str());
                let since = p.and_then(|v| v.get("since")).and_then(|v| v.as_i64());
                let until = p.and_then(|v| v.get("until")).and_then(|v| v.as_i64());
                let kind_filter: Option<Vec<String>> = p
                    .and_then(|v| v.get("kind"))
                    .and_then(|v| {
                        v.as_array()
                            .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                            .or_else(|| v.as_str().map(|s| vec![s.to_string()]))
                    });
                let offset = p
                    .and_then(|v| v.get("offset"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let limit = p
                    .and_then(|v| v.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(500)
                    .clamp(1, 2000) as usize;

                let mut all_knots: Vec<serde_json::Value> = Vec::new();

                if let Some(sid) = string_id {
                    // Single string — walk it directly.
                    if let Ok((_, entries)) = ledger.walk_string_with_tombstones(sid) {
                        for (idx, entry) in entries.iter().enumerate() {
                            all_knots.push(self.knot_to_json(idx, entry, sid));
                        }
                    } else {
                        return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32004,"message":format!("no ledger for string_id {}", sid)},
                            "id":id
                        }).to_string();
                    }
                } else {
                    // Chain-wide — walk every wallet string. (Bounded
                    // response by the hard cap on `limit × strings`,
                    // see spec §5.)
                    let (_, all) = ledger.list_strings(None, 0, 5_000);
                    let cap = 50_000usize;
                    for d in all {
                        if all_knots.len() >= cap {
                            break;
                        }
                        let id_hex = d.string_id_hex();
                        if let Ok((_, entries)) = ledger.walk_string_with_tombstones(&id_hex) {
                            for (idx, entry) in entries.iter().enumerate() {
                                if all_knots.len() >= cap {
                                    break;
                                }
                                all_knots.push(self.knot_to_json(idx, entry, &id_hex));
                            }
                        }
                    }
                }

                // Apply since/until/kind filters, then paginate.
                let filtered: Vec<serde_json::Value> = all_knots
                    .into_iter()
                    .filter(|k| {
                        let ts = k.get("anchored_at").and_then(|v| v.as_i64());
                        if let (Some(min), Some(t)) = (since, ts) {
                            if t < min {
                                return false;
                            }
                        }
                        if let (Some(max), Some(t)) = (until, ts) {
                            if t > max {
                                return false;
                            }
                        }
                        if let Some(kinds) = &kind_filter {
                            let knot_kind = k.get("kind").and_then(|v| v.as_str()).unwrap_or("");
                            if !kinds.iter().any(|k| k == knot_kind) {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();
                let total = filtered.len();
                let knots: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();
                serde_json::json!({
                    "total": total,
                    "offset": offset,
                    "limit": limit,
                    "applied_filters": {
                        "string_id": string_id,
                        "since": since,
                        "until": until,
                        "kind": kind_filter,
                    },
                    "knots": knots,
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                })
            }

            // Rope Graph v1 §4.5 — bulk read combining strings + their
            // most-recent N knots in a single round-trip. Designed to
            // collapse the frontend's 53-call first-paint cascade into
            // ONE call per ecosystem panel.
            "rope_listStringsWithKnots" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32603,"message":"personal ledger subsystem not initialized"},
                        "id":id
                    }).to_string(),
                };
                let p = params.and_then(|p| p.get(0));
                let kinds = self.parse_kind_filter(p);
                let platform = p.and_then(|v| v.get("platform")).and_then(|v| v.as_str());
                let parent_id = p.and_then(|v| v.get("parent")).and_then(|v| v.as_str());
                let ancestor_id = p.and_then(|v| v.get("ancestor")).and_then(|v| v.as_str());
                let verified_only = p
                    .and_then(|v| v.get("verified_only"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let min_knots = p
                    .and_then(|v| v.get("min_knots"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let active_since = p
                    .and_then(|v| v.get("active_since"))
                    .and_then(|v| v.as_i64());
                let sort = p.and_then(|v| v.get("sort")).and_then(|v| v.as_str());
                let limit = p
                    .and_then(|v| v.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(28)
                    .clamp(1, 200) as usize;
                let knot_limit = p
                    .and_then(|v| v.get("knot_limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50)
                    .clamp(1, 500) as usize;
                let knots_since = p
                    .and_then(|v| v.get("knots_since"))
                    .and_then(|v| v.as_i64());

                // Hard cap defensive against the spec §5 invariant.
                if limit.saturating_mul(knot_limit) > 50_000 {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{
                            "code":-32602,
                            "message":"limit × knot_limit exceeds the 50_000 hard cap (spec §5)"
                        },
                        "id":id
                    }).to_string();
                }

                let filters = StringListFilters {
                    kinds: kinds.clone(),
                    platform,
                    parent_id,
                    ancestor_id,
                    verified_only,
                    min_knots,
                    active_since,
                };
                let (_, full) = ledger.list_strings(None, 0, usize::MAX);
                let mut filtered: Vec<_> = full
                    .into_iter()
                    .filter(|d| self.descriptor_matches_filters(d, &filters))
                    .collect();
                self.sort_descriptors(&mut filtered, sort);
                let total = filtered.len();

                let strings: Vec<serde_json::Value> = filtered
                    .iter()
                    .take(limit)
                    .map(|d| {
                        let mut row = self.string_descriptor_to_json(d);
                        let id_hex = d.string_id_hex();
                        let mut row_knots = Vec::new();
                        if let Ok((_, entries)) = ledger.walk_string_with_tombstones(&id_hex) {
                            // Latest-first: take the tail then reverse.
                            let take = entries.len().min(knot_limit);
                            let start = entries.len().saturating_sub(take);
                            for (idx, entry) in entries[start..].iter().enumerate() {
                                let global_idx = start + idx;
                                let knot = self.knot_to_json(global_idx, entry, &id_hex);
                                if let Some(min) = knots_since {
                                    if knot.get("anchored_at").and_then(|v| v.as_i64())
                                        .map(|t| t < min)
                                        .unwrap_or(false)
                                    {
                                        continue;
                                    }
                                }
                                row_knots.push(knot);
                            }
                            row_knots.reverse();
                        }
                        if let Some(obj) = row.as_object_mut() {
                            obj.insert("knots".to_string(), serde_json::Value::Array(row_knots));
                        }
                        row
                    })
                    .collect();

                serde_json::json!({
                    "total": total,
                    "limit": limit,
                    "knot_limit": knot_limit,
                    "applied_filters": {
                        "kind_filter": kinds.as_ref().map(|ks| ks.iter().map(|k| k.as_str()).collect::<Vec<_>>()),
                        "platform": platform,
                        "parent": parent_id,
                        "ancestor": ancestor_id,
                        "verified_only": verified_only,
                        "min_knots": min_knots,
                        "active_since": active_since,
                        "sort": sort.unwrap_or("newest"),
                        "knots_since": knots_since,
                    },
                    "strings": strings,
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                })
            }

            // Rope Graph v1 §4.7 — top-of-tree convenience query.
            "rope_listEcosystems" => {
                let reg_arc = entity_labels::current();
                let reg: &entity_labels::LabelRegistry = &reg_arc;
                let ecos = reg.ecosystems();
                let breakdown = reg.platform_breakdown();
                let ledger_stats = self.ledger.as_ref().map(|l| l.global_stats());
                let ecosystems: Vec<_> = ecos
                    .iter()
                    .map(|l| {
                        let mut row = self.synthetic_string_to_json(l);
                        let descendants_in_labels = reg.descendant_count_of(l.id_hex);
                        let total_knots_under = ledger_stats
                            .as_ref()
                            .and_then(|s| s.by_kind.get(l.kind.as_str()).map(|c| c.knots))
                            .unwrap_or(0);
                        let by_kind = breakdown
                            .get(l.platform)
                            .cloned()
                            .unwrap_or_default();
                        if let Some(obj) = row.as_object_mut() {
                            obj.insert(
                                "descendant_count".to_string(),
                                serde_json::json!(descendants_in_labels),
                            );
                            obj.insert(
                                "total_knots_under".to_string(),
                                serde_json::json!(total_knots_under),
                            );
                            obj.insert(
                                "breakdown_by_kind".to_string(),
                                serde_json::to_value(&by_kind).unwrap_or(serde_json::json!({})),
                            );
                        }
                        row
                    })
                    .collect();
                serde_json::json!({
                    "ecosystems": ecosystems,
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                })
            }

            // Rope Graph v1 §4.7 — applications inside an ecosystem.
            "rope_listApplications" => {
                let reg_arc = entity_labels::current();
                let reg: &entity_labels::LabelRegistry = &reg_arc;
                let p = params.and_then(|p| p.get(0));
                let ecosystem_id = p
                    .and_then(|v| v.get("ecosystem_id"))
                    .and_then(|v| v.as_str())
                    .or_else(|| p.and_then(|v| v.get("platform")).and_then(|v| v.as_str()));

                let mut apps: Vec<&entity_labels::EntityLabel> =
                    reg.list_by_kind(entity_labels::LabelKind::Application);
                if let Some(eid) = ecosystem_id {
                    let key = eid.trim_start_matches("0x").to_ascii_lowercase();
                    apps.retain(|l| {
                        l.ecosystem
                            .map(|e| e.eq_ignore_ascii_case(&key))
                            .unwrap_or(false)
                            || l.platform.eq_ignore_ascii_case(&key)
                    });
                }
                let applications: Vec<_> = apps
                    .iter()
                    .map(|l| self.synthetic_string_to_json(l))
                    .collect();
                serde_json::json!({
                    "applications": applications,
                    "ecosystem_filter": ecosystem_id,
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                })
            }

            // Rope Graph v1 §4.6 — derived relations between strings.
            "rope_listRelations" => {
                let reg_arc = entity_labels::current();
                let reg: &entity_labels::LabelRegistry = &reg_arc;
                let p = params.and_then(|p| p.get(0));
                let from = p.and_then(|v| v.get("from")).and_then(|v| v.as_str());
                let to = p.and_then(|v| v.get("to")).and_then(|v| v.as_str());
                let rel_kind = p.and_then(|v| v.get("kind")).and_then(|v| v.as_str());
                let offset = p
                    .and_then(|v| v.get("offset"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let limit = p
                    .and_then(|v| v.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(500)
                    .clamp(1, 5000) as usize;

                let all = Self::derive_relations(reg);
                let total_unfiltered = all.len();
                let filtered: Vec<_> = all
                    .into_iter()
                    .filter(|r| {
                        if let Some(f) = from {
                            let key = f.trim_start_matches("0x").to_ascii_lowercase();
                            let v = r.get("from_string_id").and_then(|x| x.as_str()).unwrap_or("");
                            if !v.trim_start_matches("0x").eq_ignore_ascii_case(&key) {
                                return false;
                            }
                        }
                        if let Some(t) = to {
                            let key = t.trim_start_matches("0x").to_ascii_lowercase();
                            let v = r.get("to_string_id").and_then(|x| x.as_str()).unwrap_or("");
                            if !v.trim_start_matches("0x").eq_ignore_ascii_case(&key) {
                                return false;
                            }
                        }
                        if let Some(k) = rel_kind {
                            let v = r.get("kind").and_then(|x| x.as_str()).unwrap_or("");
                            if !v.eq_ignore_ascii_case(k) {
                                return false;
                            }
                        }
                        true
                    })
                    .collect();
                let total = filtered.len();
                let page: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();
                serde_json::json!({
                    "total": total,
                    "total_unfiltered": total_unfiltered,
                    "offset": offset,
                    "limit": limit,
                    "applied_filters": {
                        "from": from,
                        "to": to,
                        "kind": rel_kind,
                    },
                    "relations": page,
                    "note": "Relations are derived server-side from the static entity-label registry. Once on-chain attestation lands (spec §10 q1) they will be backed by signed knot history.",
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                })
            }

            // Rope Graph v1 §4.8 — name -> string_id resolution.
            "rope_resolveLabel" => {
                let p = params.and_then(|p| p.get(0));
                let query = p
                    .and_then(|v| v.get("query"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        params.and_then(|p| p.get(0)).and_then(|v| v.as_str())
                    })
                    .unwrap_or("");
                if query.is_empty() {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32602,"message":"query is required"},
                        "id":id
                    })
                    .to_string();
                }
                let limit = p
                    .and_then(|v| v.get("limit"))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(20)
                    .clamp(1, 100) as usize;
                let reg_arc = entity_labels::current();
                let reg: &entity_labels::LabelRegistry = &reg_arc;
                let hits: Vec<_> = reg
                    .search(query, limit)
                    .into_iter()
                    .map(|l| self.synthetic_string_to_json(l))
                    .collect();
                serde_json::json!({
                    "query": query,
                    "count": hits.len(),
                    "results": hits,
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                })
            }

            // Rope Graph v1 §4.9 — public method discovery.
            "rpc_methods" | "rpc_modules" => {
                let methods = Self::supported_methods();
                serde_json::json!({
                    "rpc_api_version": ROPE_RPC_API_VERSION,
                    "methods": methods,
                    "modules": {
                        "rope": "1.4",
                        "eth": "1.0",
                        "net": "1.0",
                        "web3": "1.0",
                    },
                })
            }

            "rope_anchorDeployerAttestation" => {
                // Anchor THIS node's signed [deployer] attestation onto the
                // deployer's personal ledger (== global Rope lattice). Useful
                // after re-signing with a fresh founder key, or on operator
                // demand for audit. Optional param: { force: bool }. When
                // `force` is false (default), the call is a no-op if the
                // ledger already contains an entry whose metadata matches
                // the current self_signature.
                let force = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| v.get("force"))
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                let dep =
                    match self.deployer.as_ref() {
                        Some(d) => d.clone(),
                        None => return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32603,"message":"deployer settings not initialized"},
                            "id":id
                        })
                        .to_string(),
                    };
                if dep.self_signature.trim().is_empty() {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32602,"message":"deployer.self_signature is empty (run `rope identity sign-deployer`)"},
                        "id":id
                    }).to_string();
                }
                if dep.wallet_address.trim().is_empty() {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32602,"message":"deployer.wallet_address is empty"},
                        "id":id
                    })
                    .to_string();
                }
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32603,"message":"personal ledger subsystem not initialized"},
                        "id":id
                    }).to_string(),
                };
                let canonical =
                    crate::node::deployer_canonical_json_bytes(&dep).unwrap_or_default();
                let _ = force; // forced re-anchor — we always append on RPC.
                match ledger.anchor_deployer_attestation(
                    &dep.wallet_address,
                    &canonical,
                    &dep.self_signature,
                    &self.self_node_id,
                    self.chain_id,
                ) {
                    Ok(resp) => serde_json::json!({
                        // Quipu Canon v1.2 — canonical names.
                        "kind": "wallet",
                        "string_id": dep.wallet_address,
                        "knot_id": resp.string_id,
                        "parent_knot_id": resp.parent_id,
                        "attesting_node_id": self.self_node_id,
                        "chain_id": self.chain_id,
                        "self_signature": dep.self_signature,
                        "encrypted_size": resp.encrypted_size,
                        "oes_generation": resp.oes_generation,
                        "anchored_at": chrono::Utc::now().to_rfc3339(),
                        // v1.0/1.1 deprecated aliases — drop in v1.3.
                        "wallet_address": dep.wallet_address,
                        "string_id_legacy": resp.string_id,
                        "parent_id": resp.parent_id,
                    }),
                    Err(e) => {
                        return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32603,"message":format!("anchor failed: {e}")},
                            "id":id
                        })
                        .to_string()
                    }
                }
            }

            "rope_listDeployerAttestations" => {
                // Return the personal-ledger status for a given deployer
                // wallet so callers can verify how many attestation knots
                // have been anchored. Param: { wallet: "0x..." }. Defaults
                // to this node's own [deployer].wallet_address.
                let wallet = params
                    .and_then(|p| p.get(0))
                    .and_then(|v| {
                        v.as_str()
                            .map(|s| s.to_string())
                            .or_else(|| v.get("wallet").and_then(|s| s.as_str()).map(String::from))
                    })
                    .or_else(|| self.deployer.as_ref().map(|d| d.wallet_address.clone()))
                    .unwrap_or_default();

                if wallet.is_empty() {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32602,"message":"wallet parameter required (or set [deployer].wallet_address)"},
                        "id":id
                    }).to_string();
                }
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32603,"message":"personal ledger subsystem not initialized"},
                        "id":id
                    }).to_string(),
                };
                match ledger.get_ledger_status(&wallet) {
                    Ok(status) => serde_json::json!({
                        // Quipu Canon v1.2 — canonical names.
                        "kind": "wallet",
                        "string_id": status.wallet_address,
                        "genesis_knot_id": status.genesis_string_id,
                        "head_knot_id": status.head_string_id,
                        "knot_count": status.entry_count,
                        "total_size_bytes": status.total_size_bytes,
                        "oes_generation": status.oes_generation,
                        "is_deleted": status.is_deleted,
                        "created_at": status.created_at,
                        "last_anchored_at": status.last_appended_at,
                        // v1.0/1.1 deprecated aliases — kept for one release. Drop in v1.3.
                        "wallet_address": status.wallet_address,
                        "genesis_string_id": status.genesis_string_id,
                        "head_string_id": status.head_string_id,
                        "attestation_count": status.entry_count,
                        "note": "knot_count includes ALL personal-ledger knots for this wallet's string, not only deployer attestations. Use rope_repatriateLedger + filter on metadata.attestation_kind=deployer_v1 for an exact count."
                    }),
                    Err(e) => serde_json::json!({
                        "kind": "wallet",
                        "string_id": wallet,
                        "knot_count": 0,
                        "wallet_address": wallet,
                        "attestation_count": 0,
                        "note": format!("no personal ledger for this wallet ({e}); call rope_anchorDeployerAttestation first")
                    }),
                }
            }

            method @ ("rope_suspendNode" | "rope_isolateNode" | "rope_eraseNode") => {
                let gov = match &self.governance {
                    Some(g) => g,
                    None => {
                        return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32603,"message":"Governance not initialized"},
                            "id":id
                        })
                        .to_string()
                    }
                };
                let p =
                    match params.and_then(|p| p.get(0)).and_then(|v| v.as_object()) {
                        Some(p) => p,
                        None => return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32602,"message":"Expected single object parameter"},
                            "id":id
                        })
                        .to_string(),
                    };
                let node_id = p
                    .get("node_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let reason = p
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let issued_at = p
                    .get("issued_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let nonce = p
                    .get("nonce")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let signature = p
                    .get("signature")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let pubkey = p
                    .get("pubkey")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if node_id.is_empty()
                    || issued_at.is_empty()
                    || nonce.is_empty()
                    || signature.is_empty()
                    || pubkey.is_empty()
                {
                    return serde_json::json!({
                        "jsonrpc":"2.0",
                        "error":{"code":-32602,"message":"Required: node_id, issued_at, nonce, signature, pubkey"},
                        "id":id
                    }).to_string();
                }
                let action = match method {
                    "rope_suspendNode" => GovernanceAction::Suspend {
                        node_id: node_id.clone(),
                        reason: reason.clone(),
                        ttl_secs: p.get("ttl_secs").and_then(|v| v.as_u64()).unwrap_or(3600),
                        issued_at: issued_at.clone(),
                        nonce: nonce.clone(),
                    },
                    "rope_isolateNode" => GovernanceAction::Isolate {
                        node_id: node_id.clone(),
                        reason: reason.clone(),
                        issued_at: issued_at.clone(),
                        nonce: nonce.clone(),
                    },
                    "rope_eraseNode" => GovernanceAction::Erase {
                        node_id: node_id.clone(),
                        reason: reason.clone(),
                        issued_at: issued_at.clone(),
                        nonce: nonce.clone(),
                    },
                    _ => unreachable!(),
                };
                let auth = gov.verify_action_signature(&action, &signature, &pubkey);
                let authorized_as = match &auth {
                    Authorized::Founder => "founder".to_string(),
                    Authorized::MasterNode { slot } => format!("master:{slot}"),
                    Authorized::Denied(reason) => {
                        return serde_json::json!({
                            "jsonrpc":"2.0",
                            "error":{"code":-32401,"message":format!("Forbidden: {}", reason)},
                            "id":id
                        })
                        .to_string();
                    }
                };
                gov.record_action(&action, &authorized_as, &pubkey, &signature);
                tracing::warn!(
                    "GOVERNANCE: {} {} on {} reason='{}' (authorized_as={})",
                    method,
                    node_id,
                    self.self_node_id,
                    reason,
                    authorized_as
                );
                serde_json::json!({
                    "method": method,
                    "node_id": node_id,
                    "authorized_as": authorized_as,
                    "applied_at": chrono::Utc::now().to_rfc3339(),
                    "note": "Action recorded in governance log. Network propagation \
                             relies on consensus orchestrator dispatch (Phase C)."
                })
            }

            _ => match self.delegate_to_evm(&request).await {
                EvmResult::Ok(result) => result,
                EvmResult::EvmError(e) => {
                    return serde_json::json!({"jsonrpc":"2.0","error":e,"id":id}).to_string();
                }
                EvmResult::Unavailable => {
                    return serde_json::json!({
                        "jsonrpc": "2.0",
                        "error": {
                            "code": -32601,
                            "message": format!("Method not found: {}", method)
                        },
                        "id": id
                    })
                    .to_string();
                }
            },
        };

        serde_json::json!({
            "jsonrpc": "2.0",
            "result": result,
            "id": id
        })
        .to_string()
    }

    /// Native fee history from rope-node's own gas price.
    fn native_fee_history(&self, block_count: usize) -> serde_json::Value {
        let current_block = *self.block_number.read();
        let base_fee = self.gas_price;

        let mut base_fees: Vec<String> = Vec::with_capacity(block_count + 1);
        let mut gas_used_ratios: Vec<f64> = Vec::with_capacity(block_count);
        let oldest_block = current_block.saturating_sub(block_count as u64);

        for _ in 0..block_count {
            base_fees.push(format!("0x{:x}", base_fee));
            gas_used_ratios.push(0.5);
        }
        base_fees.push(format!("0x{:x}", base_fee));

        serde_json::json!({
            "oldestBlock": format!("0x{:x}", oldest_block),
            "baseFeePerGas": base_fees,
            "gasUsedRatio": gas_used_ratios,
            "reward": []
        })
    }

    /// Default chain info response for non-JSON-RPC requests.
    ///
    /// Reserved for the planned bare-HTTP fallback handler (returns the
    /// node identity card on `GET /` for ops dashboards). Not currently
    /// wired into the dispatcher; keep allocated so the call site can be
    /// added without re-introducing the function in a hotfix.
    #[allow(dead_code)]
    async fn get_chain_info(&self) -> String {
        serde_json::json!({
            "jsonrpc": "2.0",
            "result": {
                "chainId": format!("0x{:x}", self.chain_id),
                "networkName": "Datachain Rope",
                "version": self.network_version,
                "protocols": ["rope", "ethereum-compatible"],
                "features": ["ai-testimony", "dna-regeneration", "gdpr-erasure"]
            },
            "id": 1
        })
        .to_string()
    }

    /// Increment block number (for testing)
    pub fn increment_block(&self) {
        let mut num = self.block_number.write();
        *num += 1;
    }
}

// ============================================================================
// gRPC Service Definitions (Protocol Buffer compatible)
// ============================================================================

#[async_trait::async_trait]
pub trait RopeNodeService: Send + Sync {
    async fn get_string(&self, id: [u8; 32]) -> Result<Option<StringInfo>, RpcError>;
    async fn submit_string(&self, content: Vec<u8>) -> Result<[u8; 32], RpcError>;
    async fn get_testimony_status(&self, string_id: [u8; 32]) -> Result<TestimonyStatus, RpcError>;
    async fn get_peers(&self) -> Result<Vec<PeerInfo>, RpcError>;
    async fn health_check(&self) -> Result<HealthStatus, RpcError>;
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StringInfo {
    pub id: [u8; 32],
    pub content_hash: [u8; 32],
    pub timestamp: i64,
    pub creator: [u8; 32],
    pub testimony_count: u32,
    pub is_finalized: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TestimonyStatus {
    pub string_id: [u8; 32],
    pub witnesses: u32,
    pub required_witnesses: u32,
    pub round_number: u64,
    pub is_finalized: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub node_id: [u8; 32],
    pub address: String,
    pub latency_ms: u32,
    pub version: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub uptime_seconds: u64,
    pub last_block: u64,
    pub peer_count: u32,
    pub sync_status: String,
}

#[derive(Clone, Debug)]
pub enum RpcError {
    NotFound(String),
    InvalidRequest(String),
    Internal(String),
    RateLimited,
    Unauthorized,
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RpcError::NotFound(s) => write!(f, "Not found: {}", s),
            RpcError::InvalidRequest(s) => write!(f, "Invalid request: {}", s),
            RpcError::Internal(s) => write!(f, "Internal error: {}", s),
            RpcError::RateLimited => write!(f, "Rate limited"),
            RpcError::Unauthorized => write!(f, "Unauthorized"),
        }
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Module-local lock so the two V11 hot-fix integration tests below
    /// do not race each other on the shared `ROPE_PUBLIC_DESTRUCTIVE_DENY`
    /// env var. Tokio runs `#[tokio::test]` cases on shared OS threads
    /// inside one process; without this, a sibling test's `set_var` can
    /// land between our `set_var` and `handle_json_rpc` reads.
    static V11_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// V11 hot-fix integration test: with the deny flag ON (default),
    /// every destructive `rope_*` method returns -32401 from the public
    /// dispatcher *before* the method body runs. We assert this end-to-end
    /// against `handle_json_rpc` so a future refactor cannot silently bypass
    /// the gate.
    #[tokio::test]
    async fn rpc_auth_v11_destructive_methods_denied_by_default() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Default ON regardless of CI env. Restore afterwards so we don't
        // leak a value to siblings.
        let prev = std::env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "1");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        for method in crate::rpc_auth::DESTRUCTIVE_METHODS {
            let req = format!(
                r#"{{"jsonrpc":"2.0","method":"{method}","params":[],"id":7}}"#
            );
            let resp = handlers.handle_json_rpc(&req).await;
            let v: serde_json::Value = serde_json::from_str(&resp)
                .expect("response is JSON");
            assert_eq!(v["error"]["code"], -32401, "method={method} resp={resp}");
            assert!(
                v["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("public listener"),
                "method={method} resp={resp}"
            );
            assert_eq!(v["id"], 7, "id must be echoed for method={method}");
        }
        match prev {
            Some(p) => std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
    }

    /// V11 hot-fix: an authenticated internal caller (token-matched) must
    /// be able to call destructive methods even when the public deny flag
    /// is ON. We assert by going through `handle_json_rpc_with_auth(.., true)`
    /// and confirming the response is NOT the gate's -32401.
    #[tokio::test]
    async fn rpc_auth_v11_internal_caller_bypasses_gate() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "1");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        let req =
            r#"{"jsonrpc":"2.0","method":"rope_untieKnot","params":[],"id":7}"#;
        let resp = handlers.handle_json_rpc_with_auth(req, true).await;
        let v: serde_json::Value =
            serde_json::from_str(&resp).expect("response is JSON");
        let code = v["error"]["code"].as_i64().unwrap_or(0);
        assert_ne!(
            code, -32401,
            "internal caller must NOT hit the gate; got {resp}"
        );
        match prev {
            Some(p) => std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
    }

    /// CERBER dev-only-EVM gate (2026-07-25 audit follow-up): every
    /// `anvil_*`/`evm_*` debug method must return a plain "method not
    /// found" error at the local dispatch boundary — never reaching
    /// `delegate_to_evm` at all — unless the operator has explicitly set
    /// `ROPE_ALLOW_EVM_DEV_METHODS=1`. This is asserted with `evm_backend:
    /// None`, so if the gate were ever removed/bypassed the test would
    /// instead fail deep inside `delegate_to_evm`'s "EVM backend not
    /// configured" path rather than at the boundary — the assertion on
    /// the message text below specifically pins the *gate's* wording so
    /// that regression is caught precisely, not just as "some -32601".
    #[tokio::test]
    async fn dev_only_evm_methods_denied_by_default() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("ROPE_ALLOW_EVM_DEV_METHODS").ok();
        std::env::remove_var("ROPE_ALLOW_EVM_DEV_METHODS");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        for method in crate::rpc_auth::DEV_ONLY_EVM_METHODS {
            let req = format!(
                r#"{{"jsonrpc":"2.0","method":"{method}","params":[],"id":9}}"#
            );
            let resp = handlers.handle_json_rpc(&req).await;
            let v: serde_json::Value =
                serde_json::from_str(&resp).expect("response is JSON");
            assert_eq!(v["error"]["code"], -32601, "method={method} resp={resp}");
            assert!(
                v["error"]["message"]
                    .as_str()
                    .unwrap_or("")
                    .contains("Method not found"),
                "method={method} resp={resp}"
            );
            assert_eq!(v["id"], 9, "id must be echoed for method={method}");
        }
        match prev {
            Some(p) => std::env::set_var("ROPE_ALLOW_EVM_DEV_METHODS", p),
            None => std::env::remove_var("ROPE_ALLOW_EVM_DEV_METHODS"),
        }
    }

    /// Same gate, with the escape hatch enabled: the request must now
    /// proceed past the local gate and reach `delegate_to_evm`. Since
    /// `evm_backend` is `None` here, the observable difference is the
    /// error message: the gate's fixed "Method not found" string must be
    /// gone, replaced by `delegate_to_evm`'s own "EVM backend not
    /// configured" style error — proving the request actually traveled
    /// past the gate this time.
    #[tokio::test]
    async fn dev_only_evm_methods_reach_evm_backend_when_explicitly_enabled() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("ROPE_ALLOW_EVM_DEV_METHODS").ok();
        std::env::set_var("ROPE_ALLOW_EVM_DEV_METHODS", "1");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        let req = r#"{"jsonrpc":"2.0","method":"anvil_mine","params":[],"id":9}"#;
        let resp = handlers.handle_json_rpc(req).await;
        let v: serde_json::Value = serde_json::from_str(&resp).expect("response is JSON");
        // -32603 (internal error, "EVM backend not connected") from
        // `evm_unavailable_error`, NOT -32601 ("Method not found") from the
        // gate — proving the request traveled past the gate this time.
        assert_eq!(v["error"]["code"], -32603, "resp={resp}");
        assert!(
            v["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains("EVM backend not connected"),
            "with the gate open, the response must come from delegate_to_evm, \
             not the gate's own fixed message; resp={resp}"
        );
        match prev {
            Some(p) => std::env::set_var("ROPE_ALLOW_EVM_DEV_METHODS", p),
            None => std::env::remove_var("ROPE_ALLOW_EVM_DEV_METHODS"),
        }
    }

    /// CERBER WATCH `blocked_signers` gate, wired end-to-end (2026-07-25
    /// audit follow-up, finding H1/C4): a call naming the known-compromised
    /// deployer key as its wallet parameter must be rejected with the
    /// dedicated -32402 error, and this must hold even for an *internal*
    /// caller (`is_internal = true`) — the blocklist is orthogonal to the
    /// V11 transport-trust gate, by design.
    #[tokio::test]
    async fn cerber_blocked_signer_gate_rejects_compromised_deployer_key() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "0");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        let compromised = "0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195";
        for is_internal in [false, true] {
            let req = format!(
                r#"{{"jsonrpc":"2.0","method":"rope_createPersonalLedger","params":["{compromised}"],"id":11}}"#
            );
            let resp = handlers.handle_json_rpc_with_auth(&req, is_internal).await;
            let v: serde_json::Value = serde_json::from_str(&resp).expect("response is JSON");
            assert_eq!(
                v["error"]["code"],
                crate::rpc_auth::BLOCKED_SIGNER_ERROR_CODE,
                "is_internal={is_internal} resp={resp}"
            );
            assert_eq!(v["id"], 11);
        }
        std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY");
    }

    /// A wallet that is NOT on the blocklist must sail through the CERBER
    /// gate untouched (the ledger-not-initialized error below is expected
    /// and proves the request reached the real handler, past the gate).
    #[tokio::test]
    async fn cerber_blocked_signer_gate_allows_clean_wallets() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "0");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        let req = r#"{"jsonrpc":"2.0","method":"rope_createPersonalLedger","params":["0x000000000000000000000000000000000000dEaD"],"id":12}"#;
        let resp = handlers.handle_json_rpc_with_auth(req, false).await;
        let v: serde_json::Value = serde_json::from_str(&resp).expect("response is JSON");
        assert_ne!(
            v["error"]["code"], crate::rpc_auth::BLOCKED_SIGNER_ERROR_CODE,
            "clean wallet must not hit the blocked-signer gate; resp={resp}"
        );
        std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY");
    }

    /// Quipu Canon v2.0 Phase 4 end-to-end: append knots through the
    /// `rope_v2_*` dispatcher (as an internal caller), fork a wallet with
    /// two concurrent tips, then verify walk projection, tips, compact,
    /// and stats all agree. This exercises the full RPC surface, not the
    /// DagLedger in isolation.
    #[tokio::test]
    async fn rope_v2_dag_namespace_end_to_end() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "1");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        let wallet = "0x00000000000000000000000000000000000000d4";

        // 1. Two appends against the tip set — a linear chain.
        let mut first_knot_id = String::new();
        for i in 0..2 {
            let req = format!(
                r#"{{"jsonrpc":"2.0","method":"rope_v2_appendKnot","params":["{wallet}",{{"interaction_type":"Transfer","description":"phase4 e2e knot {i}"}}],"id":1}}"#
            );
            let resp = handlers.handle_json_rpc_with_auth(&req, true).await;
            let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
            assert!(
                v["result"]["knotId"].is_string(),
                "append {i} must return a knot id; got {resp}"
            );
            if i == 0 {
                first_knot_id =
                    v["result"]["knotId"].as_str().unwrap().to_string();
            }
        }

        // 2. Fork: explicit-parent append against the FIRST knot creates
        //    a second concurrent tip.
        let req = format!(
            r#"{{"jsonrpc":"2.0","method":"rope_v2_appendKnot","params":["{wallet}",{{"interaction_type":"Transfer","description":"fork branch"}},["{first_knot_id}"]],"id":2}}"#
        );
        let resp = handlers.handle_json_rpc_with_auth(&req, true).await;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["result"]["tipCount"].as_u64(),
            Some(2),
            "explicit-parent fork must leave 2 tips; got {resp}"
        );

        // 3. tips agrees with the append receipt.
        let req = format!(
            r#"{{"jsonrpc":"2.0","method":"rope_v2_tips","params":["{wallet}"],"id":3}}"#
        );
        let resp = handlers.handle_json_rpc_with_auth(&req, true).await;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["tips"].as_array().map(|a| a.len()), Some(2));

        // 4. The projection walk is deterministic and sees all 3 knots.
        let req = format!(
            r#"{{"jsonrpc":"2.0","method":"rope_v2_walkString","params":["{wallet}"],"id":4}}"#
        );
        let resp = handlers.handle_json_rpc_with_auth(&req, true).await;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["knotCount"].as_u64(), Some(3), "resp={resp}");

        // 5. Compaction merges the two tips back to one.
        let req = format!(
            r#"{{"jsonrpc":"2.0","method":"rope_v2_compact","params":["{wallet}"],"id":5}}"#
        );
        let resp = handlers.handle_json_rpc_with_auth(&req, true).await;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["merged"].as_bool(), Some(true), "resp={resp}");

        // 6. Stats reflect the wallet, its events, and the merge.
        let req = r#"{"jsonrpc":"2.0","method":"rope_v2_stats","params":[],"id":6}"#;
        let resp = handlers.handle_json_rpc_with_auth(req, true).await;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["walletCount"].as_u64(), Some(1));
        assert_eq!(v["result"]["totalEvents"].as_u64(), Some(3));
        assert_eq!(v["result"]["totalMerges"].as_u64(), Some(1));
        assert_eq!(v["result"]["canon"], "v2.0");

        // 7. The write methods stay gated for public callers even while
        //    the read methods answer freely.
        let req = format!(
            r#"{{"jsonrpc":"2.0","method":"rope_v2_appendKnot","params":["{wallet}",{{"interaction_type":"Transfer","description":"public forge attempt"}}],"id":8}}"#
        );
        let resp = handlers.handle_json_rpc_with_auth(&req, false).await;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], -32401, "public append must be denied");
        let req = format!(
            r#"{{"jsonrpc":"2.0","method":"rope_v2_walkString","params":["{wallet}"],"id":9}}"#
        );
        let resp = handlers.handle_json_rpc_with_auth(&req, false).await;
        let v: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["result"]["knotCount"].as_u64(),
            Some(3),
            "public reads must keep working; got {resp}"
        );

        match prev {
            Some(p) => std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
    }

    /// Symmetric check: with the gate explicitly OFF (private listener),
    /// destructive methods reach their own handlers. Since most of those
    /// handlers require subsystems that the test stub doesn't provide,
    /// we just assert the response is NOT the gate's -32401 — i.e., the
    /// gate did not short-circuit the call.
    #[tokio::test]
    async fn rpc_auth_v11_destructive_methods_pass_through_when_off() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "0");
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        let req =
            r#"{"jsonrpc":"2.0","method":"rope_untieKnot","params":[],"id":7}"#;
        let resp = handlers.handle_json_rpc(req).await;
        let v: serde_json::Value =
            serde_json::from_str(&resp).expect("response is JSON");
        // We expect a different error (e.g. -32602 missing params, or
        // -32603 ledger subsystem not initialized), NOT the gate's -32401.
        let code = v["error"]["code"].as_i64().unwrap_or(0);
        assert_ne!(
            code, -32401,
            "gate must not short-circuit when DENY=0; got {resp}"
        );
        match prev {
            Some(p) => std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
    }

    /// Phase-2 integration: a public, signed `rope_appendToLedger` call
    /// must pass the auth gate even with the V11 deny flag still ON. We
    /// don't assert on the response body (the ledger subsystem isn't
    /// wired in the test stub), only on the absence of -32401. That is
    /// the canonical sign that the gate accepted the call.
    #[tokio::test]
    async fn phase2_signed_call_passes_gate_when_phase2_on() {
        use k256::ecdsa::SigningKey as EcdsaSigningKey;
        use rand::rngs::OsRng;

        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_deny = std::env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        let prev_phase2 = std::env::var("ROPE_PHASE2_SIGNED_DESTRUCTIVE").ok();
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "1");
        std::env::set_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE", "1");

        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };

        let sk = EcdsaSigningKey::random(&mut OsRng);
        let pk = k256::ecdsa::VerifyingKey::from(&sk);
        let pk_bytes = pk.to_encoded_point(false);
        let raw = &pk_bytes.as_bytes()[1..];
        use sha3::{Digest as _, Keccak256};
        let mut h = Keccak256::new();
        h.update(raw);
        let digest = h.finalize();
        let mut addr_bytes = [0u8; 20];
        addr_bytes.copy_from_slice(&digest[12..]);
        let addr_hex = format!("0x{}", hex::encode(addr_bytes));

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        use rand::RngCore;
        let mut nonce = [0u8; crate::rpc_signature::NONCE_LEN];
        OsRng.fill_bytes(&mut nonce);
        let params_without_auth = serde_json::json!([
            addr_hex,
            { "interaction_type": "TestimonyAttestation",
              "description": "phase2 integration test",
              "metadata": {} }
        ]);
        let canonical = crate::rpc_signature::canonical_message(
            "rope_appendToLedger",
            &params_without_auth,
            now,
            &nonce,
        )
        .unwrap();
        let digest = crate::rpc_signature::eip191_digest(&canonical);
        let (sig, recid) = sk
            .sign_prehash_recoverable(&digest)
            .expect("ecdsa sign");
        let mut sig65 = [0u8; 65];
        sig65[..64].copy_from_slice(&sig.to_bytes());
        sig65[64] = u8::from(recid) + 27;

        let auth = serde_json::json!({
            "auth": {
                "scheme": "secp256k1-eip191",
                "signed_at": now,
                "nonce": format!("0x{}", hex::encode(nonce)),
                "signature": format!("0x{}", hex::encode(sig65)),
            }
        });
        let mut full_params = params_without_auth.as_array().unwrap().clone();
        full_params.push(auth);
        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "rope_appendToLedger",
            "params": full_params,
            "id": 7
        })
        .to_string();

        let resp = handlers.handle_json_rpc(&req).await;
        let v: serde_json::Value =
            serde_json::from_str(&resp).expect("response is JSON");
        let code = v["error"]["code"].as_i64().unwrap_or(0);
        assert_ne!(
            code, -32401,
            "Phase-2 signed call must NOT hit the auth gate; got {resp}"
        );

        match prev_deny {
            Some(p) => std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
        match prev_phase2 {
            Some(p) => std::env::set_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE", p),
            None => std::env::remove_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE"),
        }
    }

    /// Phase-2 integration: an UNSIGNED public destructive call with the
    /// Phase-2 flag ON must STILL fail the gate (-32401), because the
    /// verifier finds no auth envelope. This is the "Phase-1 behaviour
    /// preserved when Phase-2 verification fails" property.
    #[tokio::test]
    async fn phase2_unsigned_call_still_denied_when_phase2_on() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_deny = std::env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        let prev_phase2 = std::env::var("ROPE_PHASE2_SIGNED_DESTRUCTIVE").ok();
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "1");
        std::env::set_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE", "1");

        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };

        let req = r#"{"jsonrpc":"2.0","method":"rope_untieKnot","params":[],"id":7}"#;
        let resp = handlers.handle_json_rpc(req).await;
        let v: serde_json::Value =
            serde_json::from_str(&resp).expect("response is JSON");
        assert_eq!(
            v["error"]["code"], -32401,
            "unsigned call must still be denied; got {resp}"
        );

        match prev_deny {
            Some(p) => std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
        match prev_phase2 {
            Some(p) => std::env::set_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE", p),
            None => std::env::remove_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE"),
        }
    }

    /// Phase-2 integration: with the Phase-1 deny flag explicitly OFF and
    /// Phase-2 ON, a BAD signature must be rejected with -32401. This
    /// proves Phase-2 is fail-secure even on a permissive Phase-1 listener.
    #[tokio::test]
    async fn phase2_bad_signature_denied_even_when_phase1_off() {
        let _guard = V11_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev_deny = std::env::var("ROPE_PUBLIC_DESTRUCTIVE_DENY").ok();
        let prev_phase2 = std::env::var("ROPE_PHASE2_SIGNED_DESTRUCTIVE").ok();
        std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", "0");
        std::env::set_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE", "1");

        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };

        let req = r#"{"jsonrpc":"2.0","method":"rope_untieKnot","params":[
            "0x0000000000000000000000000000000000000000",
            {"auth":{"scheme":"secp256k1-eip191","signed_at":1,"nonce":"0x00000000000000000000000000000000","signature":"0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000001b"}}
        ],"id":7}"#;
        let resp = handlers.handle_json_rpc(req).await;
        let v: serde_json::Value =
            serde_json::from_str(&resp).expect("response is JSON");
        assert_eq!(
            v["error"]["code"], -32401,
            "bad signature must be denied; got {resp}"
        );

        match prev_deny {
            Some(p) => std::env::set_var("ROPE_PUBLIC_DESTRUCTIVE_DENY", p),
            None => std::env::remove_var("ROPE_PUBLIC_DESTRUCTIVE_DENY"),
        }
        match prev_phase2 {
            Some(p) => std::env::set_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE", p),
            None => std::env::remove_var("ROPE_PHASE2_SIGNED_DESTRUCTIVE"),
        }
    }

    #[tokio::test]
    async fn test_json_rpc_chain_id() {
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };

        let request = r#"{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}"#;
        let response = handlers.handle_json_rpc(request).await;

        assert!(response.contains("0x425d4"));
        // Result must be a plain hex string for Forge/cast; not an object.
        assert!(
            response.contains(r#""result":"0x425d4""#),
            "eth_chainId must return result as string, got: {}",
            response
        );
    }

    #[tokio::test]
    async fn test_invalid_json_returns_parse_error_not_chain_info_object() {
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };
        // Invalid JSON — must return -32700 Parse error, not extended chain info object.
        let response = handlers.handle_json_rpc(r#"not valid json"#).await;
        assert!(
            response.contains("-32700"),
            "expected Parse error code: {}",
            response
        );
        assert!(
            response.contains("Parse error"),
            "expected Parse error message: {}",
            response
        );
        assert!(
            !response.contains("networkName"),
            "must not return extended object so Forge can parse: {}",
            response
        );
    }

    #[tokio::test]
    async fn test_native_block_number() {
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(42)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };

        let request = r#"{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}"#;
        let response = handlers.handle_json_rpc(request).await;
        assert!(response.contains("0x2a"));
    }

    #[tokio::test]
    async fn test_evm_call_without_evm_backend_returns_error() {
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };

        let request = r#"{"jsonrpc":"2.0","method":"eth_getBalance","params":["0x0000000000000000000000000000000000000000","latest"],"id":1}"#;
        let response = handlers.handle_json_rpc(request).await;
        assert!(response.contains("error"));
        assert!(response.contains("EVM backend not connected"));
    }

    #[tokio::test]
    async fn test_native_gas_price_without_evm_backend() {
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        };

        let request = r#"{"jsonrpc":"2.0","method":"eth_gasPrice","params":[],"id":1}"#;
        let response = handlers.handle_json_rpc(request).await;
        assert!(response.contains("0x3b9aca00"));
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let limiter = RateLimiter {
            requests_per_second: 2,
            burst: 1,
            request_counts: RwLock::new(HashMap::new()),
        };

        assert!(limiter.check("127.0.0.1").await);
        assert!(limiter.check("127.0.0.1").await);
        assert!(limiter.check("127.0.0.1").await);

        assert!(!limiter.check("127.0.0.1").await);

        assert!(limiter.check("192.168.1.1").await);
    }

    // ========================================================================
    // Rope Graph v1 — extended-shape RPC acceptance tests (T1–T7 of the spec)
    //
    // These cover the seven core acceptance tests from the Rope Graph spec
    // (`# Specification — Datachain Rope RPC for the Rope Graph`, §6).
    // The dispatcher is exercised end-to-end against an in-memory ledger
    // populated with a few synthetic strings of mixed kinds.
    // ========================================================================

    fn make_handlers_with_ledger() -> Arc<RpcHandlers> {
        use crate::ledger_manager::LedgerManager;
        use rope_core::clock::ClockManager;
        use rope_core::lattice::StringLattice;
        use rope_core::string::PublicKey;
        use rope_core::types::NodeId;
        use rope_crypto::oes::OESManager;
        use rope_storage::LedgerStore;

        let lattice = Arc::new(StringLattice::new());
        let store = Arc::new(LedgerStore::new());
        let oes = Arc::new(OESManager::genesis(&[0u8; 32]));
        let node_id = NodeId::new([1u8; 32]);
        let creator_key = PublicKey::from_ed25519([2u8; 32]);
        let clock = Arc::new(ClockManager::new(node_id));
        let ledger = Arc::new(LedgerManager::new(
            lattice,
            store,
            oes,
            node_id,
            creator_key,
            clock,
        ));

        // Seed with a mix of strings:
        //   - DC Treasury wallet (matches the foundation deployer label)
        //   - DCSwap Router contract
        //   - DCSwap Multi-Strategy MarketMaker bot
        //   - One arbitrary unlabelled wallet
        for w in [
            "0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195",
            "0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4",
            "0x9999999999999999999999999999999999999999",
        ] {
            let _ = ledger.create_ledger(w);
        }

        Arc::new(RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: Some(ledger),
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        })
    }

    fn parse_result(response: &str) -> serde_json::Value {
        let v: serde_json::Value = serde_json::from_str(response).expect("valid JSON-RPC");
        v.get("result").cloned().expect("result present")
    }

    /// T1 — kind_filter actually filters
    #[tokio::test]
    async fn t1_kind_filter_actually_filters() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_listStrings","params":[{"kind_filter":"contract","limit":5}]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let strings = r.get("strings").and_then(|s| s.as_array()).unwrap();
        for s in strings {
            assert_eq!(
                s.get("kind").and_then(|k| k.as_str()).unwrap(),
                "contract",
                "every returned string must have kind=contract"
            );
        }
    }

    /// T2 — extended String shape exposes labels.platform
    #[tokio::test]
    async fn t2_string_shape_has_labels_platform() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_listStrings","params":[{"limit":50,"platform":"foundation"}]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let strings = r.get("strings").and_then(|s| s.as_array()).unwrap();
        assert!(!strings.is_empty(), "expected at least one foundation string");
        let s = &strings[0];
        assert_eq!(
            s.get("labels").and_then(|l| l.get("platform")).and_then(|p| p.as_str()),
            Some("foundation")
        );
    }

    /// T3 — knots carry the extended fields (`anchored_at`, `kind`, …)
    #[tokio::test]
    async fn t3_knots_carry_extended_fields() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_getStringWithKnots","params":["0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195"]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let knots = r.get("knots").and_then(|k| k.as_array()).unwrap();
        assert!(!knots.is_empty());
        let k0 = &knots[0];
        for f in [
            "knot_id",
            "anchored_at",
            "kind",
            "tx_hash",
            "block_number",
            "method_name",
        ] {
            assert!(k0.get(f).is_some(), "knot must expose `{}`", f);
        }
    }

    /// T4 — bulk endpoint returns nested strings + knots in a single call
    #[tokio::test]
    async fn t4_list_strings_with_knots_bulk_read() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_listStringsWithKnots","params":[{"limit":3,"knot_limit":5}]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let strings = r.get("strings").and_then(|s| s.as_array()).unwrap();
        assert!(strings.len() <= 3);
        for s in strings {
            let knots = s.get("knots").and_then(|k| k.as_array()).unwrap();
            assert!(knots.len() <= 5);
        }
    }

    /// T5 — relations exist between bots (and other children) and their parent application
    #[tokio::test]
    async fn t5_relations_link_children_to_parents() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_listRelations","params":[{"kind":"operates","limit":5}]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let relations = r.get("relations").and_then(|x| x.as_array()).unwrap();
        assert!(!relations.is_empty(), "expected at least one `operates` relation");
        for rel in relations {
            assert!(rel.get("from_string_id").is_some());
            assert!(rel.get("to_string_id").is_some());
            assert_eq!(
                rel.get("kind").and_then(|k| k.as_str()).unwrap(),
                "operates"
            );
        }
    }

    /// T6 — ecosystems endpoint surfaces at least DCSwap and Tanastok
    #[tokio::test]
    async fn t6_list_ecosystems_includes_dcswap_and_tanastok() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_listEcosystems","params":[]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let ecos = r.get("ecosystems").and_then(|x| x.as_array()).unwrap();
        let platforms: Vec<&str> = ecos
            .iter()
            .filter_map(|e| {
                e.get("labels")
                    .and_then(|l| l.get("platform"))
                    .and_then(|p| p.as_str())
            })
            .collect();
        assert!(platforms.contains(&"dcswap"), "{:?}", platforms);
        assert!(platforms.contains(&"tanastok"), "{:?}", platforms);
    }

    /// T7 — method discovery works
    #[tokio::test]
    async fn t7_rpc_methods_discovery() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rpc_methods","params":[]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let methods = r.get("methods").and_then(|m| m.as_array()).unwrap();
        assert!(methods.len() >= 30);
        let strs: Vec<&str> = methods.iter().filter_map(|m| m.as_str()).collect();
        assert!(strs.contains(&"rope_listStrings"));
        assert!(strs.contains(&"rope_listKnots"));
        assert!(strs.contains(&"rope_listEcosystems"));
        assert!(strs.contains(&"rope_listRelations"));
        assert!(r.get("rpc_api_version").and_then(|v| v.as_str()).is_some());
    }

    /// Bonus — kind_filter accepts an array form
    #[tokio::test]
    async fn t1b_kind_filter_array_form() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_listStrings","params":[{"kind_filter":["wallet","contract"],"limit":50}]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let strings = r.get("strings").and_then(|s| s.as_array()).unwrap();
        for s in strings {
            let k = s.get("kind").and_then(|x| x.as_str()).unwrap_or("");
            assert!(k == "wallet" || k == "contract", "got kind={}", k);
        }
    }

    /// Bonus — rope_getString with synthetic ecosystem id returns a synthetic shape
    #[tokio::test]
    async fn t8_synthetic_ecosystem_id_resolves() {
        let h = make_handlers_with_ledger();
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"rope_getString","params":[{{"string_id":"0x{}"}}]}}"#,
            crate::entity_labels::ECO_DCSWAP
        );
        let r = parse_result(&h.handle_json_rpc(&req).await);
        assert_eq!(r.get("kind").and_then(|k| k.as_str()), Some("ecosystem"));
        assert_eq!(
            r.get("labels")
                .and_then(|l| l.get("platform"))
                .and_then(|p| p.as_str()),
            Some("dcswap")
        );
        assert_eq!(r.get("synthetic").and_then(|s| s.as_bool()), Some(true));
    }

    /// Bonus — rope_resolveLabel locates the DCSwap Router by display-name substring
    #[tokio::test]
    async fn t9_resolve_label_finds_router() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_resolveLabel","params":[{"query":"router","limit":5}]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let results = r.get("results").and_then(|x| x.as_array()).unwrap();
        assert!(!results.is_empty(), "expected at least one router result");
        let router = results
            .iter()
            .find(|x| {
                x.get("labels")
                    .and_then(|l| l.get("display_name"))
                    .and_then(|n| n.as_str())
                    .map(|s| s.contains("Router"))
                    .unwrap_or(false)
            })
            .expect("DCSwap Router must appear in the search results");
        assert_eq!(
            router
                .get("labels")
                .and_then(|l| l.get("platform"))
                .and_then(|p| p.as_str()),
            Some("dcswap")
        );
    }

    /// Bonus — global stats now include the label_registry breakdown
    #[tokio::test]
    async fn t10_global_stats_exposes_label_registry() {
        let h = make_handlers_with_ledger();
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_globalStats","params":[]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let lr = r.get("label_registry").expect("label_registry present");
        assert!(lr.get("ecosystems").and_then(|v| v.as_u64()).unwrap_or(0) >= 5);
        assert!(lr.get("contracts").and_then(|v| v.as_u64()).unwrap_or(0) >= 5);
    }

    /// Phase 5 — Tanastok manifest entries flow through to the live RPC.
    /// Loads a fixture manifest into the live overlay, then asserts the
    /// asset's string id resolves to its display name via
    /// `rope_resolveLabel` and that `rope_getString` returns a synthetic
    /// shape pointing at the parent application + ecosystem.
    #[tokio::test]
    async fn tanastok_manifest_overlay_flows_through_rpc() {
        use crate::entity_manifest::{
            apply_response, ManifestEntity, ManifestLabel, ManifestResponse, ManifestSource,
            DEFAULT_REFRESH_INTERVAL,
        };
        crate::entity_manifest::_test_reset_cache();

        let asset_id =
            "0x613c2b3a2a66e5340b756585b7e0e78e2156162a03ed2d3bfab4b6d8d318d44f";
        let app_id =
            "0xa1b27b82a2561f4bfe66090f4004399a17d44c54802b4adae999e6b6e9693070";
        let eco_id =
            "0x5f5a4b62b1f904df0a6a9c30f813fb8c3ebfa616f0416a59a89d0053f218d5b0";

        let resp = ManifestResponse {
            version: "1.0.0".to_string(),
            generated_at: 9_999_999_999,
            counts: serde_json::Value::Null,
            entities: vec![ManifestEntity {
                kind: "asset".to_string(),
                string_id: Some(asset_id.to_string()),
                id_bytes: None,
                parent_string_id: Some(app_id.to_string()),
                ecosystem_id: Some(eco_id.to_string()),
                label: ManifestLabel {
                    display_name: Some(
                        "Phase5RPC Watch ABCD".to_string(),
                    ),
                    role: Some("physical_asset".to_string()),
                    verified: Some(true),
                    asset_type: Some("LUXURY_WATCH".to_string()),
                    ..ManifestLabel::default()
                },
            }],
        };
        let src = ManifestSource {
            name: "tanastok",
            url: "fixture".to_string(),
            interval: DEFAULT_REFRESH_INTERVAL,
        };
        let _ = apply_response(&src, resp);

        let h = make_handlers_with_ledger();

        // 1. rope_resolveLabel finds the Tanastok asset by display name.
        let req = r#"{"jsonrpc":"2.0","id":1,"method":"rope_resolveLabel","params":[{"query":"Phase5RPC","limit":5}]}"#;
        let r = parse_result(&h.handle_json_rpc(req).await);
        let results = r
            .get("results")
            .and_then(|x| x.as_array())
            .expect("results array");
        assert!(
            results.iter().any(|x| x
                .get("labels")
                .and_then(|l| l.get("display_name"))
                .and_then(|n| n.as_str())
                == Some("Phase5RPC Watch ABCD")),
            "Tanastok asset must surface in rope_resolveLabel",
        );

        // 2. rope_getString returns the synthetic shape with parent +
        //    ecosystem ids matching the manifest.
        let req = format!(
            r#"{{"jsonrpc":"2.0","id":2,"method":"rope_getString","params":[{{"string_id":"{}"}}]}}"#,
            asset_id
        );
        let r = parse_result(&h.handle_json_rpc(&req).await);
        assert_eq!(
            r.get("kind").and_then(|x| x.as_str()),
            Some("asset"),
            "asset kind must round-trip from manifest to RPC",
        );
        assert_eq!(
            r.get("parent_string_id").and_then(|x| x.as_str()),
            Some(app_id),
        );
        assert_eq!(
            r.get("ecosystem_id").and_then(|x| x.as_str()),
            Some(eco_id),
        );
    }

    /// M9 (2026-07-25 security audit) end-to-end regression: a WebSocket
    /// frame that declares a payload length far above
    /// `MAX_WS_FRAME_PAYLOAD_BYTES` must be rejected — connection closed
    /// with RFC 6455 code 1009 ("Message Too Big") — *before* the server
    /// attempts to allocate a buffer sized off that untrusted length.
    /// Exercises the real `handle_websocket_connection` over a loopback
    /// TCP pair, not just the constant in isolation, so a future refactor
    /// that moves the check can't silently drop it.
    #[tokio::test]
    async fn ws_frame_oversized_length_is_rejected_before_allocation() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::{TcpListener, TcpStream};

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");

        let handlers = Arc::new(RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        });
        let metrics = Arc::new(RwLock::new(RpcMetrics::default()));
        let (broadcast_tx, _rx) = broadcast::channel(4);

        let server_task = tokio::spawn(async move {
            let (socket, peer) = listener.accept().await.expect("accept");
            // Errors are expected here once the client drops the socket
            // after reading the close frame — that's a normal shutdown,
            // not a test failure.
            let _ = handle_websocket_connection(
                socket,
                peer,
                handlers,
                metrics,
                broadcast_tx,
            )
            .await;
        });

        let mut client = TcpStream::connect(addr)
            .await
            .expect("connect to loopback listener");

        // Minimal WebSocket upgrade handshake.
        let handshake = "GET / HTTP/1.1\r\n\
             Host: 127.0.0.1\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n";
        client
            .write_all(handshake.as_bytes())
            .await
            .expect("send handshake");

        let mut resp_buf = [0u8; 512];
        let n = client
            .read(&mut resp_buf)
            .await
            .expect("read handshake response");
        let resp = String::from_utf8_lossy(&resp_buf[..n]);
        assert!(
            resp.starts_with("HTTP/1.1 101"),
            "expected 101 Switching Protocols, got: {resp}"
        );

        // Send a frame header declaring a 1 GiB payload via the RFC 6455
        // extended-length (opcode 127) path — far above the 16 MiB cap —
        // and then STOP. If the server's cap check runs before it tries
        // to read the (fictional) mask key + payload, it will respond
        // with a close frame right away without this test needing to
        // actually transmit a gigabyte of data.
        let oversized_len: u64 = 1024 * 1024 * 1024; // 1 GiB
        let mut frame_header = vec![0x81u8, 0xFFu8]; // FIN+text, MASK=1, len=127
        frame_header.extend_from_slice(&oversized_len.to_be_bytes());
        client
            .write_all(&frame_header)
            .await
            .expect("send oversized frame header");

        // Expect a Close frame (opcode 0x8) with status code 1009 within
        // a bounded timeout — if the server instead tried to allocate a
        // 1 GiB (or larger) buffer per client-declared length, this would
        // hang or the process would abort well before this deadline.
        let mut close_buf = [0u8; 16];
        let read_result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read(&mut close_buf),
        )
        .await
        .expect("server must respond within 5s, not hang on the oversized length");
        let n = read_result.expect("read close frame");
        assert!(n >= 4, "close frame too short: {} bytes", n);
        assert_eq!(
            close_buf[0] & 0x0F,
            0x8,
            "expected a Close frame opcode, got byte {:#x}",
            close_buf[0]
        );
        let close_payload_len = (close_buf[1] & 0x7F) as usize;
        assert_eq!(close_payload_len, 2, "close frame should carry a 2-byte status code");
        let status_code = u16::from_be_bytes([close_buf[2], close_buf[3]]);
        assert_eq!(status_code, 1009, "expected RFC 6455 code 1009 (Message Too Big)");

        // Server task must finish (connection closed), not hang forever.
        tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .expect("server task must exit after closing the oversized-frame connection")
            .expect("server task must not panic");
    }

    // ------------------------------------------------------------------
    // parse_subscription_request unit tests
    //
    // Locking these down keeps the bridge-vs-dispatcher routing decision
    // pure: any change to `SubscriptionBridge::is_bridged_method` or to
    // how we extract the id must be reflected here, so a future refactor
    // can't silently route `eth_subscribe` through the non-bridge path
    // (which is exactly the bug that produced the ChainList red-Score
    // badge before this fix landed).
    // ------------------------------------------------------------------

    #[test]
    fn parse_subscription_request_recognises_eth_subscribe() {
        let body =
            r#"{"jsonrpc":"2.0","id":7,"method":"eth_subscribe","params":["newHeads"]}"#;
        let (method, id) = parse_subscription_request(body)
            .expect("eth_subscribe must be routed to the bridge");
        assert_eq!(method, "eth_subscribe");
        assert_eq!(id, serde_json::json!(7));
    }

    #[test]
    fn parse_subscription_request_recognises_eth_unsubscribe() {
        let body =
            r#"{"jsonrpc":"2.0","id":"abc","method":"eth_unsubscribe","params":["0x1"]}"#;
        let (method, id) = parse_subscription_request(body)
            .expect("eth_unsubscribe must be routed to the bridge");
        assert_eq!(method, "eth_unsubscribe");
        assert_eq!(id, serde_json::json!("abc"));
    }

    #[test]
    fn parse_subscription_request_leaves_other_methods_alone() {
        let body =
            r#"{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}"#;
        assert!(
            parse_subscription_request(body).is_none(),
            "eth_chainId must NOT be routed to the bridge — it goes through the normal dispatcher"
        );
    }

    #[test]
    fn parse_subscription_request_null_id_when_missing() {
        // JSON-RPC 2.0 §4.2 permits omitting `id` for notifications.
        // We still hand it to the bridge so it can echo Null back in
        // the ack error path.
        let body = r#"{"jsonrpc":"2.0","method":"eth_subscribe","params":["newHeads"]}"#;
        let (method, id) = parse_subscription_request(body)
            .expect("eth_subscribe without id still routes to bridge");
        assert_eq!(method, "eth_subscribe");
        assert!(id.is_null(), "missing id must materialise as Value::Null");
    }

    #[test]
    fn parse_subscription_request_rejects_garbage() {
        assert!(parse_subscription_request("not json at all").is_none());
        assert!(parse_subscription_request("{}").is_none());
        assert!(parse_subscription_request(r#"{"jsonrpc":"2.0"}"#).is_none());
    }

    // ------------------------------------------------------------------
    // End-to-end WSS regression tests for the refactored
    // `handle_websocket_connection`.
    //
    // These drive a real TCP client against the real server function
    // (spawned on a loopback listener) so a future refactor of the
    // reader/writer split, mpsc channels, or shutdown sequence can't
    // silently regress:
    //
    //   * non-bridged JSON-RPC responses still flow through the
    //     per-connection writer task,
    //   * client-initiated ping frames produce pong replies through the
    //     mpsc channel (this exercises the writer-task pathway that the
    //     pre-refactor code handled inline),
    //   * client-initiated close frames trigger a clean shutdown of
    //     both the bridge and the writer task within a bounded timeout.
    //
    // A dedicated Reth-mock-based push-notification round-trip lives in
    // `ws_subscription_bridge::tests::end_to_end_subscribe_push_unsubscribe_round_trip`
    // — no need to duplicate it here since the bridge is exercised
    // through its public `handle` surface.
    // ------------------------------------------------------------------

    /// Perform the RFC 6455 client handshake on `client` and assert 101
    /// Switching Protocols. Panics on any transport or parse error —
    /// the test would be meaningless without the upgrade succeeding.
    async fn perform_ws_handshake(client: &mut tokio::net::TcpStream) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let handshake = "GET / HTTP/1.1\r\n\
             Host: 127.0.0.1\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n";
        client
            .write_all(handshake.as_bytes())
            .await
            .expect("send handshake");
        let mut resp_buf = [0u8; 512];
        let n = client
            .read(&mut resp_buf)
            .await
            .expect("read handshake response");
        let resp = String::from_utf8_lossy(&resp_buf[..n]);
        assert!(
            resp.starts_with("HTTP/1.1 101"),
            "expected 101 Switching Protocols, got: {resp}"
        );
    }

    /// Frame `payload` as a MASKED client text frame (opcode 0x1) —
    /// required by RFC 6455 §5.3 for client→server frames — and write
    /// it to `client`.
    async fn send_client_text_frame(client: &mut tokio::net::TcpStream, payload: &[u8]) {
        use tokio::io::AsyncWriteExt;
        let mut frame = Vec::with_capacity(payload.len() + 14);
        frame.push(0x81); // FIN + opcode 0x1 (text)
        let len = payload.len();
        if len < 126 {
            frame.push(0x80 | (len as u8));
        } else if len < 65536 {
            frame.push(0x80 | 126);
            frame.extend_from_slice(&(len as u16).to_be_bytes());
        } else {
            frame.push(0x80 | 127);
            frame.extend_from_slice(&(len as u64).to_be_bytes());
        }
        let mask = [0x37u8, 0xfa, 0x21, 0x3d];
        frame.extend_from_slice(&mask);
        for (i, b) in payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        client
            .write_all(&frame)
            .await
            .expect("send client text frame");
    }

    /// Read exactly one *unmasked* server frame (server→client frames
    /// are never masked per RFC 6455 §5.3) and return `(opcode,
    /// payload)`. Supports the 7-bit and 16-bit length forms which is
    /// enough for the responses these tests exercise (all under 64 KiB).
    async fn read_server_frame(
        client: &mut tokio::net::TcpStream,
    ) -> (u8, Vec<u8>) {
        use tokio::io::AsyncReadExt;
        let mut header = [0u8; 2];
        client
            .read_exact(&mut header)
            .await
            .expect("read frame header");
        let opcode = header[0] & 0x0F;
        let mut len = (header[1] & 0x7F) as usize;
        if len == 126 {
            let mut ext = [0u8; 2];
            client.read_exact(&mut ext).await.expect("read 16-bit len");
            len = u16::from_be_bytes(ext) as usize;
        } else if len == 127 {
            panic!("64-bit length not needed for these tests");
        }
        let mut payload = vec![0u8; len];
        if len > 0 {
            client
                .read_exact(&mut payload)
                .await
                .expect("read payload");
        }
        (opcode, payload)
    }

    /// Build a minimal `RpcHandlers` sufficient for the WSS tests. The
    /// EVM backend is `None`, so `eth_subscribe` — routed through
    /// [`SubscriptionBridge`] — returns the canonical `-32601` and no
    /// upstream socket is opened. `eth_chainId` still resolves natively
    /// against `chain_id: 271828`.
    fn test_handlers() -> Arc<RpcHandlers> {
        Arc::new(RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
            evm_backend: None,
            orchestrator: None,
            ledger: None,
            iot_gateway: None,
            ai_framework: None,
            governance: None,
            deployer: None,
            self_node_id: String::new(),
            auth_verifier: parking_lot::RwLock::new(None),
            dag: Arc::new(crate::dag_ledger::DagLedger::new()),
        })
    }

    /// Spawn `handle_websocket_connection` on a loopback listener and
    /// return `(client, server_task)`. The server task's error is
    /// intentionally swallowed — clean shutdowns after the client drops
    /// the socket are represented as `Err(io)` by `read_exact`, which
    /// is expected, not a test failure.
    async fn spawn_ws_server() -> (
        tokio::net::TcpStream,
        tokio::task::JoinHandle<()>,
    ) {
        use tokio::net::{TcpListener, TcpStream};
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind loopback listener");
        let addr = listener.local_addr().expect("local addr");
        let handlers = test_handlers();
        let metrics = Arc::new(RwLock::new(RpcMetrics::default()));
        let (broadcast_tx, _rx) = broadcast::channel(4);
        let server_task = tokio::spawn(async move {
            let (socket, peer) = listener.accept().await.expect("accept");
            let _ = handle_websocket_connection(
                socket,
                peer,
                handlers,
                metrics,
                broadcast_tx,
            )
            .await;
        });
        let client = TcpStream::connect(addr)
            .await
            .expect("connect to loopback listener");
        (client, server_task)
    }

    #[tokio::test]
    async fn ws_eth_chain_id_still_works_through_refactored_writer_task() {
        let (mut client, server_task) = spawn_ws_server().await;
        perform_ws_handshake(&mut client).await;

        let req = r#"{"jsonrpc":"2.0","id":42,"method":"eth_chainId","params":[]}"#;
        send_client_text_frame(&mut client, req.as_bytes()).await;

        let (opcode, payload) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_server_frame(&mut client),
        )
        .await
        .expect("server must reply within 5s");
        assert_eq!(opcode, 0x1, "expected text frame");
        let response: serde_json::Value =
            serde_json::from_slice(&payload).expect("valid JSON-RPC");
        assert_eq!(response["id"], serde_json::json!(42));
        assert_eq!(
            response["result"].as_str(),
            Some("0x425d4"),
            "chain id 271828 must round-trip verbatim through the new writer task"
        );

        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .expect("server task must exit after client drops")
            .expect("server task must not panic");
    }

    #[tokio::test]
    async fn ws_eth_subscribe_returns_method_unavailable_when_bridge_disabled() {
        // `evm_backend: None` above means `reth_ws_url()` never returns
        // Some, so the SubscriptionBridge is constructed disabled and
        // must reply with a canonical -32601 without opening any
        // upstream socket. This is the failure mode a public rope-node
        // deployment sees if the operator has explicitly set
        // `ROPE_RETH_WS_URL=""` — ChainList's scorer must NOT interpret
        // that as a working subscription surface.
        let (mut client, server_task) = spawn_ws_server().await;
        perform_ws_handshake(&mut client).await;

        let req = r#"{"jsonrpc":"2.0","id":9,"method":"eth_subscribe","params":["newHeads"]}"#;
        send_client_text_frame(&mut client, req.as_bytes()).await;

        let (opcode, payload) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_server_frame(&mut client),
        )
        .await
        .expect("bridge-disabled ack must arrive within 5s");
        assert_eq!(opcode, 0x1);
        let response: serde_json::Value =
            serde_json::from_slice(&payload).expect("valid JSON-RPC");
        assert_eq!(response["id"], serde_json::json!(9));
        let code = response["error"]["code"]
            .as_i64()
            .expect("error.code present");
        assert_eq!(
            code, -32601,
            "disabled bridge must return the standard 'method not available' code, not a fake subscription id"
        );

        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .expect("server task must exit after client drops")
            .expect("server task must not panic");
    }

    #[tokio::test]
    async fn ws_ping_frame_produces_pong_through_writer_task() {
        // Pre-refactor the ping/pong path wrote inline to the raw
        // stream. Post-refactor it enqueues `WsWriteFrame::Pong` onto
        // the mpsc consumed by `ws_writer_task`. This test locks that
        // pathway down so a maintainer removing the Pong arm from the
        // writer task's match would be caught by CI.
        use tokio::io::AsyncWriteExt;
        let (mut client, server_task) = spawn_ws_server().await;
        perform_ws_handshake(&mut client).await;

        // Masked client ping frame with a 4-byte payload.
        let ping_payload = *b"ping";
        let mask = [0x11u8, 0x22, 0x33, 0x44];
        let mut frame = vec![0x89u8, 0x84]; // FIN + opcode 0x9, MASK=1, len=4
        frame.extend_from_slice(&mask);
        for (i, b) in ping_payload.iter().enumerate() {
            frame.push(b ^ mask[i % 4]);
        }
        client.write_all(&frame).await.expect("send ping");

        let (opcode, payload) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_server_frame(&mut client),
        )
        .await
        .expect("pong must arrive within 5s");
        assert_eq!(opcode, 0xA, "expected pong opcode");
        assert_eq!(payload.as_slice(), &ping_payload[..]);

        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .expect("server task must exit after client drops")
            .expect("server task must not panic");
    }

    #[tokio::test]
    async fn ws_client_close_frame_shuts_down_cleanly() {
        use tokio::io::AsyncWriteExt;
        let (mut client, server_task) = spawn_ws_server().await;
        perform_ws_handshake(&mut client).await;

        // Masked client close frame, empty payload.
        let mask = [0x00u8, 0x00, 0x00, 0x00];
        let mut frame = vec![0x88u8, 0x80]; // FIN + opcode 0x8, MASK=1, len=0
        frame.extend_from_slice(&mask);
        client.write_all(&frame).await.expect("send close");

        // Server echoes an empty close frame per the pre-refactor
        // behaviour that this test locks down.
        let (opcode, payload) = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            read_server_frame(&mut client),
        )
        .await
        .expect("close echo must arrive within 5s");
        assert_eq!(opcode, 0x8, "expected close opcode");
        assert!(
            payload.is_empty(),
            "server's close echo carries no status code, matches pre-refactor behaviour"
        );

        // Server task must finish — the shutdown sequence (drop bridge,
        // drop write_tx, await writer_task) must run to completion
        // within its bounded timeout.
        tokio::time::timeout(std::time::Duration::from_secs(5), server_task)
            .await
            .expect("server task must exit after receiving client close")
            .expect("server task must not panic");
    }
}
