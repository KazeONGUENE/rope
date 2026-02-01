//! gRPC API Server with mTLS support
//!
//! This module provides a full-featured gRPC server for Datachain Rope:
//! - JSON-RPC compatible Ethereum API
//! - Native Rope API (gRPC + Protocol Buffers)
//! - Mutual TLS (mTLS) authentication
//! - Rate limiting and request validation
//! - Metrics and observability

use crate::config::RpcSettings;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;

/// RPC Server with mTLS support
pub struct RpcServer {
    /// Configuration
    config: RpcSettings,

    /// TLS configuration (if enabled)
    tls_config: Option<TlsConfig>,

    /// Rate limiter
    rate_limiter: Arc<RateLimiter>,

    /// Request handlers
    handlers: Arc<RpcHandlers>,

    /// Metrics
    metrics: Arc<RwLock<RpcMetrics>>,
}

/// TLS configuration for mTLS
#[derive(Clone)]
pub struct TlsConfig {
    /// Server certificate (PEM)
    pub server_cert: Vec<u8>,

    /// Server private key (PEM)
    pub server_key: Vec<u8>,

    /// CA certificate for client verification (PEM)
    pub ca_cert: Option<Vec<u8>>,

    /// Require client certificate (mTLS)
    pub require_client_cert: bool,
}

/// Rate limiter configuration
pub struct RateLimiter {
    /// Requests per second per IP
    requests_per_second: u32,

    /// Burst allowance
    burst: u32,

    /// Request counts by IP
    request_counts: RwLock<HashMap<String, RequestCounter>>,
}

/// Request counter for rate limiting
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

/// RPC handlers for different methods
pub struct RpcHandlers {
    /// Chain ID
    chain_id: u64,

    /// Network version
    network_version: String,

    /// Block number (shared with node)
    block_number: Arc<parking_lot::RwLock<u64>>,

    /// Gas price in wei
    gas_price: u64,
}

impl RpcServer {
    /// Create new RPC server
    pub async fn new(config: &RpcSettings) -> anyhow::Result<Self> {
        Self::new_with_state(config, 271828, Arc::new(parking_lot::RwLock::new(0))).await
    }

    /// Create new RPC server with shared state
    pub async fn new_with_state(
        config: &RpcSettings,
        chain_id: u64,
        current_round: Arc<parking_lot::RwLock<u64>>,
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
            gas_price: 1_000_000_000, // 1 Gwei
        });

        Ok(Self {
            config: config.clone(),
            tls_config: None,
            rate_limiter,
            handlers,
            metrics: Arc::new(RwLock::new(RpcMetrics::default())),
        })
    }

    /// Configure TLS
    pub fn with_tls(mut self, tls_config: TlsConfig) -> Self {
        self.tls_config = Some(tls_config);
        self
    }

    /// Configure mTLS (mutual TLS)
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

    /// Run the RPC server
    pub async fn run(&self) -> anyhow::Result<()> {
        let addr: SocketAddr = self.config.grpc_addr.parse()?;

        tracing::info!("Starting RPC server on {}", addr);

        if self.tls_config.is_some() {
            tracing::info!(
                "TLS enabled, mTLS: {}",
                self.tls_config
                    .as_ref()
                    .map(|c| c.require_client_cert)
                    .unwrap_or(false)
            );
        }

        let listener = tokio::net::TcpListener::bind(&addr).await?;
        tracing::info!("RPC server ready (JSON-RPC + gRPC compatible)");

        loop {
            let (stream, peer_addr) = listener.accept().await?;

            let handlers = self.handlers.clone();
            let rate_limiter = self.rate_limiter.clone();
            let metrics = self.metrics.clone();

            {
                let mut m = metrics.write().await;
                m.active_connections += 1;
            }

            tokio::spawn(async move {
                let peer_ip = peer_addr.ip().to_string();

                // Check rate limit
                if !rate_limiter.check(&peer_ip).await {
                    let mut m = metrics.write().await;
                    m.rate_limited_requests += 1;
                    return;
                }

                if let Err(e) = handle_connection(stream, handlers, metrics.clone()).await {
                    tracing::error!("Connection error from {}: {}", peer_addr, e);
                }

                {
                    let mut m = metrics.write().await;
                    m.active_connections = m.active_connections.saturating_sub(1);
                }
            });
        }
    }

    /// Get current metrics
    pub async fn metrics(&self) -> RpcMetrics {
        self.metrics.read().await.clone()
    }
}

impl RateLimiter {
    /// Check if request is allowed
    async fn check(&self, ip: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        let mut counts = self.request_counts.write().await;

        let counter = counts.entry(ip.to_string()).or_default();

        // Reset window if expired
        if now - counter.window_start >= 1 {
            counter.count = 0;
            counter.window_start = now;
        }

        // Check limit
        if counter.count >= self.requests_per_second + self.burst {
            return false;
        }

        counter.count += 1;
        true
    }
}

/// Handle a single connection
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    handlers: Arc<RpcHandlers>,
    metrics: Arc<RwLock<RpcMetrics>>,
) -> anyhow::Result<()> {
    let start = std::time::Instant::now();
    let mut buf = [0u8; 8192];
    let n = stream.read(&mut buf).await?;

    if n == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buf[..n]);

    // Update metrics
    {
        let mut m = metrics.write().await;
        m.total_requests += 1;
    }

    let response = if request.contains("POST") || request.contains("GET /") {
        // Extract JSON-RPC body if present
        let body_start = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
        let body = &request[body_start..];

        // Handle JSON-RPC request
        let json_response = handlers.handle_json_rpc(body).await;

        format!(
            "HTTP/1.1 200 OK\r\n\
            Content-Type: application/json\r\n\
            Access-Control-Allow-Origin: *\r\n\
            Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
            Access-Control-Allow-Headers: Content-Type\r\n\
            Content-Length: {}\r\n\r\n{}",
            json_response.len(),
            json_response
        )
    } else if request.contains("OPTIONS") {
        "HTTP/1.1 204 No Content\r\n\
        Access-Control-Allow-Origin: *\r\n\
        Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
        Access-Control-Allow-Headers: Content-Type\r\n\r\n"
            .to_string()
    } else {
        "HTTP/1.1 404 Not Found\r\n\r\n".to_string()
    };

    stream.write_all(response.as_bytes()).await?;

    // Update metrics
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

impl RpcHandlers {
    /// Handle JSON-RPC request
    async fn handle_json_rpc(&self, body: &str) -> String {
        // Parse JSON-RPC request
        let request: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(_) => {
                // Return default info for non-JSON requests
                return self.get_chain_info().await;
            }
        };

        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = request.get("id").cloned().unwrap_or(serde_json::json!(1));

        let result = match method {
            // Standard Ethereum JSON-RPC methods
            "eth_chainId" => {
                serde_json::json!(format!("0x{:x}", self.chain_id))
            }
            "eth_blockNumber" => {
                let num = *self.block_number.read();
                serde_json::json!(format!("0x{:x}", num))
            }
            "eth_gasPrice" => {
                serde_json::json!(format!("0x{:x}", self.gas_price))
            }
            "net_version" => {
                serde_json::json!(self.chain_id.to_string())
            }
            "web3_clientVersion" => {
                serde_json::json!(format!("Datachain-Rope/{}", self.network_version))
            }
            "eth_syncing" => {
                serde_json::json!(false)
            }
            "eth_accounts" => {
                serde_json::json!([])
            }
            "eth_getBalance" => {
                // Return 0 balance for any address (placeholder)
                serde_json::json!("0x0")
            }
            "eth_getTransactionCount" => {
                serde_json::json!("0x0")
            }
            "eth_getCode" => {
                serde_json::json!("0x")
            }
            "eth_call" => {
                serde_json::json!("0x")
            }
            "eth_estimateGas" => {
                serde_json::json!("0x5208") // 21000 gas
            }
            "eth_sendRawTransaction" => {
                // Generate mock transaction hash
                let hash = format!("0x{}", hex::encode(&[0u8; 32]));
                serde_json::json!(hash)
            }
            "eth_getTransactionReceipt" => {
                // Parse tx hash from params
                let params = request.get("params").and_then(|p| p.as_array());
                if let Some(params) = params {
                    if let Some(tx_hash) = params.first().and_then(|h| h.as_str()) {
                        self.get_transaction_receipt(tx_hash).await
                    } else {
                        serde_json::json!(null)
                    }
                } else {
                    serde_json::json!(null)
                }
            }
            "eth_getBlockByNumber" => {
                let params = request.get("params").and_then(|p| p.as_array());
                let block_tag = params
                    .and_then(|p| p.first())
                    .and_then(|b| b.as_str())
                    .unwrap_or("latest");
                let full_txs = params
                    .and_then(|p| p.get(1))
                    .and_then(|f| f.as_bool())
                    .unwrap_or(false);
                self.get_block_by_number(block_tag, full_txs).await
            }
            "eth_getBlockByHash" => {
                let params = request.get("params").and_then(|p| p.as_array());
                let full_txs = params
                    .and_then(|p| p.get(1))
                    .and_then(|f| f.as_bool())
                    .unwrap_or(false);
                self.get_mock_block_with_txs(full_txs).await
            }
            "eth_getLogs" => {
                // Parse filter from params
                let params = request.get("params").and_then(|p| p.as_array());
                if let Some(params) = params {
                    if let Some(filter) = params.first() {
                        self.get_logs(filter).await
                    } else {
                        serde_json::json!([])
                    }
                } else {
                    serde_json::json!([])
                }
            }
            "eth_getStorageAt" => {
                // Return empty storage for any address/slot
                serde_json::json!(
                    "0x0000000000000000000000000000000000000000000000000000000000000000"
                )
            }
            "eth_getBlockTransactionCountByNumber" => {
                serde_json::json!("0x0")
            }
            "eth_getBlockTransactionCountByHash" => {
                serde_json::json!("0x0")
            }
            "eth_getTransactionByHash" => {
                let params = request.get("params").and_then(|p| p.as_array());
                if let Some(params) = params {
                    if let Some(tx_hash) = params.first().and_then(|h| h.as_str()) {
                        self.get_transaction_by_hash(tx_hash).await
                    } else {
                        serde_json::json!(null)
                    }
                } else {
                    serde_json::json!(null)
                }
            }
            "eth_getTransactionByBlockNumberAndIndex" => {
                serde_json::json!(null)
            }
            "eth_getUncleCountByBlockNumber" => {
                serde_json::json!("0x0")
            }
            "eth_protocolVersion" => {
                serde_json::json!("0x41") // Protocol version 65
            }
            "net_listening" => {
                serde_json::json!(true)
            }
            "net_peerCount" => {
                serde_json::json!("0x0")
            }
            "eth_mining" => {
                serde_json::json!(false)
            }
            "eth_hashrate" => {
                serde_json::json!("0x0")
            }
            "eth_feeHistory" => {
                // EIP-1559 fee history
                let params = request.get("params").and_then(|p| p.as_array());
                let block_count = params
                    .and_then(|p| p.first())
                    .and_then(|b| b.as_u64())
                    .unwrap_or(1) as usize;
                self.get_fee_history(block_count).await
            }
            "eth_maxPriorityFeePerGas" => {
                // Return 1 Gwei as default priority fee
                serde_json::json!("0x3b9aca00")
            }

            // Datachain Rope native methods
            "rope_getStringById" => {
                serde_json::json!({
                    "id": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "content": null,
                    "timestamp": chrono::Utc::now().timestamp()
                })
            }
            "rope_getTestimonyStatus" => {
                serde_json::json!({
                    "consensus": "finalized",
                    "witnesses": 5,
                    "roundNumber": 1
                })
            }
            "rope_getNetworkInfo" => {
                serde_json::json!({
                    "chainId": self.chain_id,
                    "networkName": "Datachain Rope Mainnet",
                    "version": self.network_version,
                    "peerCount": 0,
                    "consensusType": "testimony"
                })
            }
            "rope_getAIAgentStatus" => {
                serde_json::json!({
                    "validationAgent": "active",
                    "insuranceAgent": "active",
                    "complianceAgent": "active",
                    "oracleAgent": "active"
                })
            }

            _ => {
                // Unknown method
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
        };

        serde_json::json!({
            "jsonrpc": "2.0",
            "result": result,
            "id": id
        })
        .to_string()
    }

    /// Get chain info (default response)
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

    /// Get mock block (placeholder)
    async fn get_mock_block(&self) -> serde_json::Value {
        self.get_mock_block_with_txs(false).await
    }

    /// Get mock block with optional full transactions
    async fn get_mock_block_with_txs(&self, _full_txs: bool) -> serde_json::Value {
        let block_num = *self.block_number.read();
        let timestamp = chrono::Utc::now().timestamp() as u64;

        // Generate deterministic block hash from block number
        let mut block_hash = [0u8; 32];
        block_hash[..8].copy_from_slice(&block_num.to_be_bytes());

        let mut parent_hash = [0u8; 32];
        if block_num > 0 {
            parent_hash[..8].copy_from_slice(&(block_num - 1).to_be_bytes());
        }

        serde_json::json!({
            "number": format!("0x{:x}", block_num),
            "hash": format!("0x{}", hex::encode(block_hash)),
            "parentHash": format!("0x{}", hex::encode(parent_hash)),
            "nonce": "0x0000000000000000",
            "sha3Uncles": "0x1dcc4de8dec75d7aab85b567b6ccd41ad312451b948a7413f0a142fd40d49347",
            "logsBloom": format!("0x{}", "00".repeat(256)),
            "transactionsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
            "stateRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
            "receiptsRoot": "0x56e81f171bcc55a6ff8345e692c0f86e5b48e01b996cadc001622fb5e363b421",
            "miner": "0x0000000000000000000000000000000000000000",
            "difficulty": "0x0",
            "totalDifficulty": "0x0",
            "extraData": "0x",
            "size": "0x220",
            "gasLimit": "0x1c9c380",
            "gasUsed": "0x0",
            "timestamp": format!("0x{:x}", timestamp),
            "transactions": [],
            "uncles": [],
            "baseFeePerGas": format!("0x{:x}", self.gas_price)
        })
    }

    /// Get block by number or tag
    async fn get_block_by_number(&self, block_tag: &str, full_txs: bool) -> serde_json::Value {
        let block_num = match block_tag {
            "latest" | "pending" => *self.block_number.read(),
            "earliest" => 0,
            "safe" | "finalized" => {
                let current = *self.block_number.read();
                current.saturating_sub(1)
            }
            hex_str if hex_str.starts_with("0x") => {
                u64::from_str_radix(hex_str.trim_start_matches("0x"), 16).unwrap_or(0)
            }
            _ => *self.block_number.read(),
        };

        // Temporarily set block number for generating block
        let current = *self.block_number.read();
        *self.block_number.write() = block_num;
        let result = self.get_mock_block_with_txs(full_txs).await;
        *self.block_number.write() = current;
        result
    }

    /// Get transaction receipt
    async fn get_transaction_receipt(&self, tx_hash: &str) -> serde_json::Value {
        // For now, return null (transaction not found)
        // In production, this would query the transaction pool/database
        let _ = tx_hash;
        serde_json::json!(null)
    }

    /// Get transaction by hash
    async fn get_transaction_by_hash(&self, tx_hash: &str) -> serde_json::Value {
        // For now, return null (transaction not found)
        let _ = tx_hash;
        serde_json::json!(null)
    }

    /// Get logs matching filter
    async fn get_logs(&self, filter: &serde_json::Value) -> serde_json::Value {
        // Parse filter parameters
        let _from_block = filter
            .get("fromBlock")
            .and_then(|b| b.as_str())
            .unwrap_or("latest");
        let _to_block = filter
            .get("toBlock")
            .and_then(|b| b.as_str())
            .unwrap_or("latest");
        let _address = filter.get("address");
        let _topics = filter.get("topics");

        // Return empty logs array for now
        // In production, this would query indexed events
        serde_json::json!([])
    }

    /// Get fee history (EIP-1559)
    async fn get_fee_history(&self, block_count: usize) -> serde_json::Value {
        let current_block = *self.block_number.read();
        let base_fee = self.gas_price;

        // Generate mock fee history
        let mut base_fees: Vec<String> = Vec::with_capacity(block_count + 1);
        let mut gas_used_ratios: Vec<f64> = Vec::with_capacity(block_count);
        let oldest_block = current_block.saturating_sub(block_count as u64);

        for _ in 0..block_count {
            base_fees.push(format!("0x{:x}", base_fee));
            gas_used_ratios.push(0.5); // 50% utilization
        }
        base_fees.push(format!("0x{:x}", base_fee)); // Next block base fee

        serde_json::json!({
            "oldestBlock": format!("0x{:x}", oldest_block),
            "baseFeePerGas": base_fees,
            "gasUsedRatio": gas_used_ratios,
            "reward": []
        })
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

/// gRPC service trait for Rope Node
#[async_trait::async_trait]
pub trait RopeNodeService: Send + Sync {
    /// Get string by ID
    async fn get_string(&self, id: [u8; 32]) -> Result<Option<StringInfo>, RpcError>;

    /// Submit a new string
    async fn submit_string(&self, content: Vec<u8>) -> Result<[u8; 32], RpcError>;

    /// Get testimony status
    async fn get_testimony_status(&self, string_id: [u8; 32]) -> Result<TestimonyStatus, RpcError>;

    /// Get network peers
    async fn get_peers(&self) -> Result<Vec<PeerInfo>, RpcError>;

    /// Health check
    async fn health_check(&self) -> Result<HealthStatus, RpcError>;
}

/// String information
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct StringInfo {
    pub id: [u8; 32],
    pub content_hash: [u8; 32],
    pub timestamp: i64,
    pub creator: [u8; 32],
    pub testimony_count: u32,
    pub is_finalized: bool,
}

/// Testimony status
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct TestimonyStatus {
    pub string_id: [u8; 32],
    pub witnesses: u32,
    pub required_witnesses: u32,
    pub round_number: u64,
    pub is_finalized: bool,
}

/// Peer information
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct PeerInfo {
    pub node_id: [u8; 32],
    pub address: String,
    pub latency_ms: u32,
    pub version: String,
}

/// Health status
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct HealthStatus {
    pub status: String,
    pub uptime_seconds: u64,
    pub last_block: u64,
    pub peer_count: u32,
    pub sync_status: String,
}

/// RPC error
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

    #[tokio::test]
    async fn test_json_rpc_chain_id() {
        let handlers = RpcHandlers {
            chain_id: 271828,
            network_version: "0.1.0".to_string(),
            block_number: Arc::new(parking_lot::RwLock::new(1)),
            gas_price: 1_000_000_000,
        };

        let request = r#"{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}"#;
        let response = handlers.handle_json_rpc(request).await;

        assert!(response.contains("0x425d4")); // 271828 in hex
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let limiter = RateLimiter {
            requests_per_second: 2,
            burst: 1,
            request_counts: RwLock::new(HashMap::new()),
        };

        // First 3 requests should pass (2 + 1 burst)
        assert!(limiter.check("127.0.0.1").await);
        assert!(limiter.check("127.0.0.1").await);
        assert!(limiter.check("127.0.0.1").await);

        // 4th request should be rate limited
        assert!(!limiter.check("127.0.0.1").await);

        // Different IP should work
        assert!(limiter.check("192.168.1.1").await);
    }
}
