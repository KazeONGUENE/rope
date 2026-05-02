//! EVM Backend — wire-compatible client to the EVM execution layer.
//!
//! ## Architectural role
//!
//! `rope-node` is the master execution layer for Datachain Rope. The EVM
//! backend is an **optional verifier / executor** that:
//!
//! - Executes EVM operations (`eth_call`, `eth_sendRawTransaction`,
//!   `eth_getBalance`, `eth_getCode`, …)
//! - Persists EVM state independently of the rope-node consensus layer
//! - Never faces the public directly — `rope-node` is the only client
//!
//! All EVM calls flow:
//!
//! ```text
//! Client → rope-node RPC → EvmBackend → EVM execution layer → response
//! ```
//!
//! `rope-node` wraps each notarized transaction into a `RopeString` for
//! consensus notarization on the String Lattice DAG, regardless of which
//! EVM execution engine sits behind this client.
//!
//! ## Production execution layer
//!
//! As of 2026-03-31, the production execution layer is **Reth v1.11.2**
//! (blue-green deployment across `rope-vps:8595` ↔ `anvil-vps:8595`, with
//! IPFS state replication). See `reth-blue-green-ipfs-architecture.mdc` and
//! `reth-migration-2026-03-12.mdc`.
//!
//! Before 2026-03-12 the execution layer was Anvil; the migration was
//! transparent at this layer because Reth and Anvil are wire-compatible at
//! the JSON-RPC protocol level. The legacy module name `anvil_backend` and
//! its types (`AnvilBackend`, `AnvilConfig`, `AnvilHealth`) were renamed to
//! the protocol-neutral `EvmBackend` family on 2026-05-02 to match the
//! current production reality and the Quipu Primitive Canon v1.1
//! (which speaks of a generic EVM execution layer rather than any one
//! specific implementation).

use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

/// Health status of the connected EVM execution layer.
#[derive(Clone, Debug)]
pub struct EvmBackendHealth {
    pub reachable: bool,
    pub chain_id: Option<u64>,
    pub block_number: Option<u64>,
    pub last_check: Instant,
    pub consecutive_failures: u32,
}

/// Configuration for the EVM backend connection.
#[derive(Clone, Debug)]
pub struct EvmBackendConfig {
    /// EVM execution layer's internal HTTP URL (never exposed publicly).
    /// In production this points at the local Reth instance.
    pub url: String,
    /// Request timeout
    pub timeout: Duration,
    /// Maximum consecutive failures before marking unhealthy
    pub max_failures: u32,
    /// Health check interval
    pub health_interval: Duration,
    /// Expected chain ID (must match rope-node's chain ID)
    pub expected_chain_id: u64,
}

impl Default for EvmBackendConfig {
    fn default() -> Self {
        Self {
            // Production Reth listens on 8595 (per reth-blue-green-ipfs-architecture.mdc).
            url: "http://127.0.0.1:8595".to_string(),
            timeout: Duration::from_secs(300),
            max_failures: 5,
            health_interval: Duration::from_secs(30),
            expected_chain_id: 271828,
        }
    }
}

/// HTTP JSON-RPC client to the local EVM execution layer (Reth in production).
pub struct EvmBackend {
    config: EvmBackendConfig,
    client: reqwest::Client,
    healthy: AtomicBool,
    request_counter: AtomicU64,
    health: Arc<RwLock<EvmBackendHealth>>,
}

impl EvmBackend {
    /// Create a new EVM backend client.
    pub fn new(config: EvmBackendConfig) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(10)
            .build()?;

        Ok(Self {
            config,
            client,
            healthy: AtomicBool::new(false),
            request_counter: AtomicU64::new(1),
            health: Arc::new(RwLock::new(EvmBackendHealth {
                reachable: false,
                chain_id: None,
                block_number: None,
                last_check: Instant::now(),
                consecutive_failures: 0,
            })),
        })
    }

    /// Check if the EVM backend is healthy and reachable.
    pub fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }

    /// Get current health status.
    pub async fn health(&self) -> EvmBackendHealth {
        self.health.read().await.clone()
    }

    /// Initialize the backend — verify the EVM layer is reachable and chain
    /// IDs match.
    pub async fn initialize(&self) -> anyhow::Result<()> {
        info!("Initializing EVM backend at {}", self.config.url);

        let chain_id = self.get_chain_id().await?;
        if chain_id != self.config.expected_chain_id {
            anyhow::bail!(
                "EVM backend chain ID mismatch: expected {}, got {}",
                self.config.expected_chain_id,
                chain_id
            );
        }

        let block_number = self.get_block_number().await?;

        {
            let mut h = self.health.write().await;
            h.reachable = true;
            h.chain_id = Some(chain_id);
            h.block_number = Some(block_number);
            h.last_check = Instant::now();
            h.consecutive_failures = 0;
        }
        self.healthy.store(true, Ordering::Relaxed);

        info!(
            "EVM backend initialized: chainId={}, block={}",
            chain_id, block_number
        );
        Ok(())
    }

    /// Send a raw JSON-RPC request to the EVM backend and return the result.
    pub async fn json_rpc(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.request_counter.fetch_add(1, Ordering::Relaxed);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id
        });

        debug!("EVM RPC: {} (id={})", method, id);

        let response = self
            .client
            .post(&self.config.url)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                self.record_failure();
                anyhow::anyhow!("EVM backend request failed: {}", e)
            })?;

        let body: Value = response.json().await.map_err(|e| {
            self.record_failure();
            anyhow::anyhow!("EVM backend response parse failed: {}", e)
        })?;

        self.record_success();

        if let Some(error) = body.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            anyhow::bail!("EVM backend RPC error {}: {}", code, message);
        }

        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Forward a complete JSON-RPC request object to the EVM backend
    /// (preserving the client's id).
    pub async fn forward_request(&self, request: &Value) -> anyhow::Result<Value> {
        let method = request
            .get("method")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown");

        debug!("Forwarding to EVM backend: {}", method);

        let response = self
            .client
            .post(&self.config.url)
            .json(request)
            .send()
            .await
            .map_err(|e| {
                self.record_failure();
                anyhow::anyhow!("EVM backend forward failed: {}", e)
            })?;

        let body: Value = response.json().await.map_err(|e| {
            self.record_failure();
            anyhow::anyhow!("EVM backend forward response parse failed: {}", e)
        })?;

        self.record_success();
        Ok(body)
    }

    // ---- EVM convenience methods ----

    pub async fn get_chain_id(&self) -> anyhow::Result<u64> {
        let result = self.json_rpc("eth_chainId", Value::Array(vec![])).await?;
        parse_hex_u64(&result)
    }

    pub async fn get_block_number(&self) -> anyhow::Result<u64> {
        let result = self
            .json_rpc("eth_blockNumber", Value::Array(vec![]))
            .await?;
        parse_hex_u64(&result)
    }

    pub async fn get_balance(&self, address: &str, block: &str) -> anyhow::Result<Value> {
        self.json_rpc("eth_getBalance", serde_json::json!([address, block]))
            .await
    }

    pub async fn get_code(&self, address: &str, block: &str) -> anyhow::Result<Value> {
        self.json_rpc("eth_getCode", serde_json::json!([address, block]))
            .await
    }

    pub async fn eth_call(&self, tx: &Value, block: &str) -> anyhow::Result<Value> {
        self.json_rpc("eth_call", serde_json::json!([tx, block]))
            .await
    }

    pub async fn send_raw_transaction(&self, raw_tx: &str) -> anyhow::Result<Value> {
        self.json_rpc("eth_sendRawTransaction", serde_json::json!([raw_tx]))
            .await
    }

    pub async fn get_transaction_receipt(&self, tx_hash: &str) -> anyhow::Result<Value> {
        self.json_rpc("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await
    }

    pub async fn get_transaction_count(&self, address: &str, block: &str) -> anyhow::Result<Value> {
        self.json_rpc(
            "eth_getTransactionCount",
            serde_json::json!([address, block]),
        )
        .await
    }

    pub async fn estimate_gas(&self, tx: &Value) -> anyhow::Result<Value> {
        self.json_rpc("eth_estimateGas", serde_json::json!([tx]))
            .await
    }

    pub async fn get_block_by_number(&self, block: &str, full_txs: bool) -> anyhow::Result<Value> {
        self.json_rpc("eth_getBlockByNumber", serde_json::json!([block, full_txs]))
            .await
    }

    pub async fn get_block_by_hash(&self, hash: &str, full_txs: bool) -> anyhow::Result<Value> {
        self.json_rpc("eth_getBlockByHash", serde_json::json!([hash, full_txs]))
            .await
    }

    pub async fn get_logs(&self, filter: &Value) -> anyhow::Result<Value> {
        self.json_rpc("eth_getLogs", serde_json::json!([filter]))
            .await
    }

    pub async fn get_storage_at(
        &self,
        address: &str,
        slot: &str,
        block: &str,
    ) -> anyhow::Result<Value> {
        self.json_rpc(
            "eth_getStorageAt",
            serde_json::json!([address, slot, block]),
        )
        .await
    }

    pub async fn get_transaction_by_hash(&self, tx_hash: &str) -> anyhow::Result<Value> {
        self.json_rpc("eth_getTransactionByHash", serde_json::json!([tx_hash]))
            .await
    }

    pub async fn get_fee_history(
        &self,
        block_count: u64,
        newest_block: &str,
        percentiles: &[f64],
    ) -> anyhow::Result<Value> {
        self.json_rpc(
            "eth_feeHistory",
            serde_json::json!([format!("0x{:x}", block_count), newest_block, percentiles]),
        )
        .await
    }

    /// Dump the complete EVM state (for backups / verification).
    ///
    /// Note: this method targets the legacy `anvil_dumpState` RPC. Anvil
    /// supported it natively; Reth does not (state snapshots on Reth are
    /// taken via the IPFS pin pipeline described in
    /// `reth-blue-green-ipfs-architecture.mdc`). On Reth, this call will
    /// return a JSON-RPC method-not-found error — that is expected and is
    /// not a defect at this layer.
    pub async fn dump_state(&self) -> anyhow::Result<Value> {
        self.long_running_rpc("anvil_dumpState", Value::Array(vec![]))
            .await
    }

    /// Load state into the EVM execution layer.
    ///
    /// Same caveat as `dump_state` — this targets `anvil_loadState` and is
    /// only operative against an Anvil execution layer. The Reth code path
    /// uses the IPFS-pinned chain-state tarball restoration flow instead.
    pub async fn load_state(&self, state: &str) -> anyhow::Result<Value> {
        self.long_running_rpc("anvil_loadState", serde_json::json!([state]))
            .await
    }

    /// Execute a JSON-RPC call with a 30-minute timeout for operations that
    /// transfer multi-GB payloads (state dump/load).
    async fn long_running_rpc(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        let id = self.request_counter.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": id
        });

        info!("EVM backend long-running RPC: {} (id={})", method, id);

        let long_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(1800))
            .connect_timeout(Duration::from_secs(10))
            .pool_max_idle_per_host(1)
            .build()?;

        let response = long_client
            .post(&self.config.url)
            .json(&request)
            .send()
            .await
            .map_err(|e| {
                self.record_failure();
                anyhow::anyhow!("EVM backend long-running request failed: {}", e)
            })?;

        let body: Value = response.json().await.map_err(|e| {
            self.record_failure();
            anyhow::anyhow!("EVM backend long-running response parse failed: {}", e)
        })?;

        self.record_success();

        if let Some(error) = body.get("error") {
            let code = error.get("code").and_then(|c| c.as_i64()).unwrap_or(-1);
            let message = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("Unknown error");
            anyhow::bail!("EVM backend RPC error {}: {}", code, message);
        }

        Ok(body.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Spawn a background health checker.
    pub fn spawn_health_checker(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let backend = Arc::clone(self);
        let interval = self.config.health_interval;

        tokio::spawn(async move {
            loop {
                tokio::time::sleep(interval).await;

                match backend.get_block_number().await {
                    Ok(block) => {
                        let mut h = backend.health.write().await;
                        h.reachable = true;
                        h.block_number = Some(block);
                        h.last_check = Instant::now();
                        h.consecutive_failures = 0;
                        backend.healthy.store(true, Ordering::Relaxed);
                    }
                    Err(e) => {
                        warn!("EVM backend health check failed: {}", e);
                        let mut h = backend.health.write().await;
                        h.consecutive_failures += 1;
                        h.last_check = Instant::now();

                        if h.consecutive_failures >= backend.config.max_failures {
                            h.reachable = false;
                            backend.healthy.store(false, Ordering::Relaxed);
                            error!(
                                "EVM backend marked UNHEALTHY after {} consecutive failures",
                                h.consecutive_failures
                            );
                        }
                    }
                }
            }
        })
    }

    fn record_failure(&self) {
        let healthy = self.healthy.load(Ordering::Relaxed);
        if healthy {
            warn!("EVM backend request failed (still healthy, tracking)");
        }
    }

    fn record_success(&self) {
        if !self.healthy.load(Ordering::Relaxed) {
            info!("EVM backend recovered");
            self.healthy.store(true, Ordering::Relaxed);
        }
    }
}

fn parse_hex_u64(value: &Value) -> anyhow::Result<u64> {
    let s = value
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Expected hex string, got {:?}", value))?;
    let s = s.trim_start_matches("0x");
    u64::from_str_radix(s, 16).map_err(|e| anyhow::anyhow!("Invalid hex: {}", e))
}

// ============================================================================
// Backwards-compatibility aliases
// ============================================================================
//
// External consumers (none currently exist outside this crate, but operators
// reading old logs and developers searching the codebase will look for the
// pre-rename names) get a transparent alias path. These aliases are
// `#[deprecated]` so that any in-repo use generates a compile warning, but
// downstream code outside the workspace continues to compile unchanged.

/// Deprecated alias for [`EvmBackend`]. Kept for source compatibility.
#[deprecated(
    since = "0.2.0",
    note = "Anvil was archived 2026-03-31. Use `EvmBackend`. \
            See reth-blue-green-ipfs-architecture.mdc."
)]
pub type AnvilBackend = EvmBackend;

/// Deprecated alias for [`EvmBackendConfig`]. Kept for source compatibility.
#[deprecated(
    since = "0.2.0",
    note = "Use `EvmBackendConfig`. \
            See reth-blue-green-ipfs-architecture.mdc."
)]
pub type AnvilConfig = EvmBackendConfig;

/// Deprecated alias for [`EvmBackendHealth`]. Kept for source compatibility.
#[deprecated(
    since = "0.2.0",
    note = "Use `EvmBackendHealth`. \
            See reth-blue-green-ipfs-architecture.mdc."
)]
pub type AnvilHealth = EvmBackendHealth;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_hex_u64() {
        assert_eq!(
            parse_hex_u64(&Value::String("0x425d4".into())).unwrap(),
            271828
        );
        assert_eq!(parse_hex_u64(&Value::String("0x0".into())).unwrap(), 0);
        assert_eq!(parse_hex_u64(&Value::String("0xff".into())).unwrap(), 255);
    }

    #[test]
    fn test_evm_backend_config_default() {
        let config = EvmBackendConfig::default();
        assert_eq!(config.expected_chain_id, 271828);
        // Default points at the production Reth port (was 8548 under Anvil).
        assert_eq!(config.url, "http://127.0.0.1:8595");
    }
}
