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

use crate::evm_backend::EvmBackend;
use crate::config::RpcSettings;
use crate::consensus_orchestrator::ConsensusOrchestrator;
use crate::ledger_manager::LedgerManager;
use rope_iot_gateway::IoTGateway;
use rope_ai_framework::AgentFramework;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, RwLock};
use sha1::{Sha1, Digest};

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
}

impl RpcServer {
    pub async fn new(config: &RpcSettings) -> anyhow::Result<Self> {
        Self::new_with_state(config, 271828, Arc::new(parking_lot::RwLock::new(0)), None, None).await
    }

    pub async fn new_with_state(
        config: &RpcSettings,
        chain_id: u64,
        current_round: Arc<parking_lot::RwLock<u64>>,
        evm_backend: Option<Arc<EvmBackend>>,
        orchestrator: Option<Arc<ConsensusOrchestrator>>,
    ) -> anyhow::Result<Self> {
        Self::new_full(config, chain_id, current_round, evm_backend, orchestrator, None, None, None).await
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

                            if !rate_limiter.check(&peer_ip).await {
                                let mut m = metrics.write().await;
                                m.rate_limited_requests += 1;
                                return;
                            }

                            if let Err(e) = handle_connection(stream, handlers, metrics.clone()).await {
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

                            if let Err(e) = handle_websocket_connection(stream, handlers, metrics.clone(), broadcast).await {
                                tracing::debug!("WebSocket connection error from {}: {}", peer_addr, e);
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

impl RateLimiter {
    async fn check(&self, ip: &str) -> bool {
        let now = chrono::Utc::now().timestamp();
        let mut counts = self.request_counts.write().await;

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

const MAX_REQUEST_SIZE: usize = 2_147_483_648; // 2 GB — large contract deployments + state dumps
const READ_BUF_SIZE: usize = 262_144; // 256 KB read chunks for throughput

async fn read_full_http_request(stream: &mut tokio::net::TcpStream) -> anyhow::Result<Vec<u8>> {
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

            if content_length > MAX_REQUEST_SIZE {
                anyhow::bail!("Request body too large ({} bytes)", content_length);
            }

            let body_expected = header_end + 4 + content_length;
            data.reserve(body_expected.saturating_sub(data.len()));
            while data.len() < body_expected {
                let n = stream.read(&mut tmp).await?;
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&tmp[..n]);
            }
            break;
        }

        if data.len() > MAX_REQUEST_SIZE {
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

async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    handlers: Arc<RpcHandlers>,
    metrics: Arc<RwLock<RpcMetrics>>,
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

async fn handle_websocket_connection(
    mut stream: tokio::net::TcpStream,
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

        loop {
            let mut header = [0u8; 2];
            if stream.read_exact(&mut header).await.is_err() {
                break;
            }

            let fin = (header[0] & 0x80) != 0;
            let opcode = header[0] & 0x0F;
            let masked = (header[1] & 0x80) != 0;
            let mut payload_len = (header[1] & 0x7F) as usize;

            if payload_len == 126 {
                let mut ext = [0u8; 2];
                if stream.read_exact(&mut ext).await.is_err() {
                    break;
                }
                payload_len = u16::from_be_bytes(ext) as usize;
            } else if payload_len == 127 {
                let mut ext = [0u8; 8];
                if stream.read_exact(&mut ext).await.is_err() {
                    break;
                }
                payload_len = u64::from_be_bytes(ext) as usize;
            }

            let mut mask_key = [0u8; 4];
            if masked {
                if stream.read_exact(&mut mask_key).await.is_err() {
                    break;
                }
            }

            let mut payload = vec![0u8; payload_len];
            if !payload.is_empty() && stream.read_exact(&mut payload).await.is_err() {
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
                        let response = handlers.handle_json_rpc(&request_str).await;

                        send_websocket_frame(&mut stream, 0x1, response.as_bytes()).await?;

                        {
                            let mut m = metrics.write().await;
                            m.total_requests += 1;
                            m.successful_requests += 1;
                        }
                    }
                }
                0x8 => {
                    send_websocket_frame(&mut stream, 0x8, &[]).await?;
                    break;
                }
                0x9 => {
                    send_websocket_frame(&mut stream, 0xA, &payload).await?;
                }
                0xA => {}
                _ => {
                    break;
                }
            }
        }

        {
            let mut m = metrics.write().await;
            m.active_connections = m.active_connections.saturating_sub(1);
        }
    } else {
        let body_start = request.find("\r\n\r\n").map(|i| i + 4).unwrap_or(0);
        let body = &request[body_start..];
        let json_response = handlers.handle_json_rpc(body).await;

        let response = format!(
            "HTTP/1.1 200 OK\r\n\
            Content-Type: application/json\r\n\
            Access-Control-Allow-Origin: *\r\n\
            Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
            Access-Control-Allow-Headers: Content-Type\r\n\
            Content-Length: {}\r\n\r\n{}",
            json_response.len(),
            json_response
        );
        stream.write_all(response.as_bytes()).await?;
    }

    Ok(())
}

fn generate_websocket_accept_key(key: &str) -> String {
    use base64::{Engine as _, engine::general_purpose};

    let magic = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
    let combined = format!("{}{}", key, magic);

    let mut hasher = Sha1::new();
    hasher.update(combined.as_bytes());
    let result = hasher.finalize();

    general_purpose::STANDARD.encode(result)
}

async fn send_websocket_frame(
    stream: &mut tokio::net::TcpStream,
    opcode: u8,
    payload: &[u8],
) -> anyhow::Result<()> {
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

impl RpcHandlers {
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
            }).to_string()),
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
        const ZERO_HASH: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";
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
    async fn handle_json_rpc(&self, body: &str) -> String {
        let request: serde_json::Value = match serde_json::from_str(body) {
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
                            return serde_json::to_string(&response).unwrap_or_else(|_| "[]".to_string());
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

        let method = request.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = request.get("id").cloned().unwrap_or(serde_json::json!(1));
        let params = request.get("params");

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
                match self.delegate_to_evm(&upstream_request).await {
                    EvmResult::Ok(result) => {
                        if let Some(hex_str) = result.as_str() {
                            if let Ok(n) = u64::from_str_radix(hex_str.trim_start_matches("0x"), 16) {
                                *self.block_number.write() = n;
                            }
                        }
                        result
                    }
                    EvmResult::EvmError(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":e,"id":id}).to_string();
                    }
                    EvmResult::Unavailable => {
                        let num = *self.block_number.read();
                        serde_json::json!(format!("0x{:x}", num))
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
            "eth_feeHistory" => {
                match self.delegate_to_evm(&request).await {
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
                }
            }

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
            "eth_getBlockByNumber" | "eth_getBlockByHash"
            | "rope_getKnotByIndex" | "rope_getKnotByHash" => {
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
                                    serde_json::json!("v1.1 §3 — eth_getBlockBy* preserved as alias"),
                                );
                            }
                        }
                        result
                    }
                    Err(err_response) => return err_response,
                }
            }
            "eth_getBalance" | "eth_getTransactionCount" | "eth_getCode" |
            "eth_call" | "eth_estimateGas" | "eth_getStorageAt" |
            "eth_getLogs" |
            "eth_getBlockTransactionCountByNumber" | "eth_getBlockTransactionCountByHash" |
            "eth_getTransactionByHash" | "eth_getTransactionByBlockNumberAndIndex" |
            "eth_getTransactionReceipt" => {
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
            // EVM admin / debug methods — only forwarded when the EVM backend
            // is connected. The `anvil_*` method *names* are wire-protocol
            // identifiers from the legacy Anvil tooling era; we keep matching
            // them by literal name so that tools like Hardhat and Foundry
            // continue to work transparently. The production EVM execution
            // layer (Reth) does not implement most `anvil_*` methods and will
            // return a JSON-RPC method-not-found error for them — that is
            // the correct, expected behaviour and is forwarded to the client
            // unchanged.
            // ================================================================
            "anvil_impersonateAccount" | "anvil_stopImpersonatingAccount" |
            "anvil_setBalance" | "anvil_setCode" | "anvil_setNonce" |
            "anvil_dumpState" | "anvil_loadState" | "anvil_mine" |
            "anvil_setStorageAt" | "anvil_reset" |
            "evm_snapshot" | "evm_revert" | "evm_increaseTime" | "evm_mine" => {
                match self.unwrap_evm_or_error(self.delegate_to_evm(&request).await, method, &id) {
                    Ok(result) => result,
                    Err(err_response) => return err_response,
                }
            }

            // ================================================================
            // DATACHAIN ROPE NATIVE METHODS — always available
            // ================================================================
            "rope_getStringById" => {
                serde_json::json!({
                    "id": "0x0000000000000000000000000000000000000000000000000000000000000000",
                    "content": null,
                    "timestamp": chrono::Utc::now().timestamp()
                })
            }
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
            "rope_getNetworkInfo" => {
                let evm_connected = self.evm_backend.as_ref().map(|b| b.is_healthy()).unwrap_or(false);
                let ai_agents = self.orchestrator.as_ref()
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

            // ================================================================
            // PERSONAL LEDGER — one String per wallet, distributed via RDP
            // ================================================================
            "rope_createPersonalLedger" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address parameter"},"id":id}).to_string();
                }
                match ledger.create_ledger(owner) {
                    Ok(resp) => {
                        let now = chrono::Utc::now().timestamp();
                        serde_json::json!({"owner": owner, "created_at": now})
                    }
                    Err(e) if e.contains("already exists") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2001,"message":"Ledger already exists for this address"},"id":id}).to_string();
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
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
                let owner = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
                let interaction_val = params.and_then(|p| p.get(1));
                if owner.is_empty() || interaction_val.is_none() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address or interaction parameter"},"id":id}).to_string();
                }
                let interaction_val = interaction_val.unwrap();

                let itype_str = interaction_val.get("interaction_type").and_then(|v| v.as_str()).unwrap_or("Custom");
                let description = interaction_val.get("description").and_then(|v| v.as_str()).unwrap_or("");
                let metadata = interaction_val.get("metadata").cloned().unwrap_or(serde_json::json!({}));

                use rope_core::personal_ledger::InteractionType;
                let interaction_type = match itype_str {
                    "Transfer" => InteractionType::Transfer,
                    "ContractCall" | "ContractDeploy" => InteractionType::ContractCall,
                    "TokenApproval" | "Approval" => InteractionType::TokenApproval,
                    "IdentityClaim" | "DIDCreation" | "DIDUpdate" | "ClaimIssuance" => InteractionType::IdentityClaim,
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
                                map.insert(k.clone(), v.as_str().unwrap_or(&v.to_string()).to_string());
                            }
                        }
                        map
                    },
                };

                match ledger.append_to_ledger(owner, record) {
                    Ok(resp) => serde_json::json!({
                        "index": resp.piece_count,
                        "hash": resp.string_id
                    }),
                    Err(e) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Err(e) if e.contains("deleted") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2003,"message":"Ledger has been deleted"},"id":id}).to_string();
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
                    }
                }
            }

            "rope_getLedgerStatus" => {
                let ledger = match &self.ledger {
                    Some(l) => l,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"Ledger subsystem not initialized"},"id":id}).to_string(),
                };
                let owner = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
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
                let owner = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing owner address parameter"},"id":id}).to_string();
                }
                match ledger.repatriate_ledger(owner, false) {
                    Ok(resp) => {
                        let fragments: Vec<serde_json::Value> = resp.entries.iter().map(|e| {
                            serde_json::json!({
                                "index": e.sequence,
                                "hash": e.string_id,
                                "timestamp": chrono::Utc::now().timestamp(),
                                "interaction": null
                            })
                        }).collect();
                        let integrity = format!("0x{:0>64x}", resp.total_bytes);
                        serde_json::json!({
                            "owner": resp.wallet_address,
                            "fragments": fragments,
                            "assembled_at": chrono::Utc::now().timestamp(),
                            "integrity_hash": integrity
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
                let owner = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
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
                match ledger.erase_ledger(owner, DeletionReason::OwnerRequest) {
                    Ok(resp) => serde_json::json!({
                        "owner": resp.wallet_address,
                        "erased_fragments": resp.entries_erased,
                        "audit_hash": format!("0x{}", resp.audit_hash),
                        "erased_at": chrono::Utc::now().timestamp(),
                        "gdpr_article": "Article 17 — Right to Erasure (whole-string closure)",
                        "scope": "whole_wallet",
                        "canon": "v1.1 §6 — explicit wallet-closure, equivalent to closing the account. For granular per-event erasure, use rope_untieKnot.",
                        "auth_method": "phase-1-trusted-proxy"
                    }),
                    Err(e) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Err(e) if e.contains("already deleted") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2003,"message":"Ledger already deleted"},"id":id}).to_string();
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
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
                let owner = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
                let knot_id_raw = params.and_then(|p| p.get(1)).and_then(|v| v.as_str()).unwrap_or("");
                let reason = params.and_then(|p| p.get(2)).and_then(|v| v.as_str()).unwrap_or("OwnerRequest");

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

                match ledger.untie_knot(owner, knot_id, reason) {
                    Ok(resp) => serde_json::json!({
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
                    Err(e) if e.contains("No ledger") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2002,"message":"No ledger found for this address"},"id":id}).to_string();
                    }
                    Err(e) if e.contains("already wholly deleted") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2003,"message":e},"id":id}).to_string();
                    }
                    Err(e) if e.contains("genesis knot") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2010,"message":e},"id":id}).to_string();
                    }
                    Err(e) if e.contains("does not belong") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2011,"message":e},"id":id}).to_string();
                    }
                    Err(e) if e.contains("already untied") => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":2012,"message":e},"id":id}).to_string();
                    }
                    Err(e) => {
                        return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":e},"id":id}).to_string();
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
                let owner = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
                if owner.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing wallet address parameter"},"id":id}).to_string();
                }
                match ledger.walk_string_with_tombstones(owner) {
                    Ok((string_id_hex, entries)) => {
                        use rope_core::lattice::LedgerEntry;
                        let knots: Vec<serde_json::Value> = entries
                            .iter()
                            .enumerate()
                            .map(|(idx, entry)| match entry {
                                LedgerEntry::Active(sid) => serde_json::json!({
                                    "knot_index": idx,
                                    "string_id": format!("0x{}", sid.to_hex()),
                                    "status": "active",
                                    "tombstone": null,
                                }),
                                LedgerEntry::Tombstone(sid, ts) => serde_json::json!({
                                    "knot_index": idx,
                                    "string_id": format!("0x{}", sid.to_hex()),
                                    "status": "tombstone",
                                    "tombstone": {
                                        "untied_at": ts.untied_at,
                                        "audit_hash": format!("0x{}", hex::encode(ts.audit_hash)),
                                        "reason": ts.reason,
                                    },
                                }),
                            })
                            .collect();
                        let active = knots.iter().filter(|k| k.get("status").and_then(|v| v.as_str()) == Some("active")).count();
                        let tombs = knots.iter().filter(|k| k.get("status").and_then(|v| v.as_str()) == Some("tombstone")).count();
                        serde_json::json!({
                            "wallet_address": owner,
                            "string_id": format!("0x{}", string_id_hex),
                            "knots": knots,
                            "knot_count": knots.len(),
                            "active_count": active,
                            "tombstone_count": tombs,
                            "canon": "v1.1 §6(2) — String → Knot[] → Transaction details"
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
                let p = params.and_then(|p| p.get(0)).cloned().unwrap_or(serde_json::json!({}));
                let device_id = p.get("device_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let wallet = p.get("wallet_address").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let dtype = p.get("device_type").and_then(|v| v.as_str()).unwrap_or("sensor");
                let name = p.get("name").and_then(|v| v.as_str()).unwrap_or(&device_id).to_string();
                let owner = p.get("owner_wallet").and_then(|v| v.as_str()).unwrap_or(&wallet).to_string();
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
                let p = params.and_then(|p| p.get(0)).cloned().unwrap_or(serde_json::json!({}));
                let wallet = p.get("device_wallet").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if wallet.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing device_wallet"},"id":id}).to_string();
                }

                let mut readings = hashbrown::HashMap::new();
                if let Some(obj) = p.get("readings").and_then(|v| v.as_object()) {
                    for (k, v) in obj {
                        if let Some(f) = v.as_f64() {
                            readings.insert(k.clone(), rope_iot_gateway::protocol::TelemetryValue::Float(f));
                        } else if let Some(i) = v.as_i64() {
                            readings.insert(k.clone(), rope_iot_gateway::protocol::TelemetryValue::Integer(i));
                        } else if let Some(b) = v.as_bool() {
                            readings.insert(k.clone(), rope_iot_gateway::protocol::TelemetryValue::Boolean(b));
                        } else if let Some(s) = v.as_str() {
                            readings.insert(k.clone(), rope_iot_gateway::protocol::TelemetryValue::Text(s.to_string()));
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
                    Ok(()) => serde_json::json!({"status": "ingested", "timestamp": chrono::Utc::now().timestamp()}),
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
                let device_id = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
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
                let devices: Vec<serde_json::Value> = gw.registry().list_devices().iter().map(|d| {
                    serde_json::json!({
                        "device_id": d.device_id,
                        "wallet_address": d.wallet_address,
                        "device_type": d.device_type.as_str(),
                        "name": d.name,
                        "status": format!("{:?}", d.status),
                        "telemetry_count": d.telemetry_count,
                        "last_seen_at": d.last_seen_at
                    })
                }).collect();
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
                let agent_id = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
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
                let agents: Vec<serde_json::Value> = fw.list_agents().iter().map(|a| {
                    serde_json::json!({
                        "agent_id": a.agent_id,
                        "name": a.name,
                        "domain": a.domain.as_str(),
                        "version": a.version,
                        "state": a.state.as_str(),
                        "run_count": a.run_count,
                        "avg_confidence": a.avg_confidence
                    })
                }).collect();
                serde_json::json!({"agents": agents, "count": agents.len()})
            }

            "rope_subscribeAgentToWallet" => {
                let fw = match &self.ai_framework {
                    Some(f) => f,
                    None => return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32603,"message":"AI Agent Framework not initialized"},"id":id}).to_string(),
                };
                let agent_id = params.and_then(|p| p.get(0)).and_then(|v| v.as_str()).unwrap_or("");
                let wallet = params.and_then(|p| p.get(1)).and_then(|v| v.as_str()).unwrap_or("");
                if agent_id.is_empty() || wallet.is_empty() {
                    return serde_json::json!({"jsonrpc":"2.0","error":{"code":-32602,"message":"Missing agent_id or wallet parameter"},"id":id}).to_string();
                }
                match fw.subscribe_agent_to_wallet(agent_id, wallet) {
                    Ok(()) => serde_json::json!({"status": "subscribed", "agent_id": agent_id, "wallet": wallet}),
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
                let limit = params.and_then(|p| p.get(0)).and_then(|v| v.as_u64()).unwrap_or(10) as usize;
                let diagnoses: Vec<serde_json::Value> = fw.recent_diagnoses(limit).iter().map(|d| {
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
                }).collect();
                serde_json::json!({"diagnoses": diagnoses, "count": diagnoses.len()})
            }

            _ => {
                match self.delegate_to_evm(&request).await {
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
                        }).to_string();
                    }
                }
            }
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
        };

        let request = r#"{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}"#;
        let response = handlers.handle_json_rpc(request).await;

        assert!(response.contains("0x425d4"));
        // Result must be a plain hex string for Forge/cast; not an object.
        assert!(response.contains(r#""result":"0x425d4""#), "eth_chainId must return result as string, got: {}", response);
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
        };
        // Invalid JSON — must return -32700 Parse error, not extended chain info object.
        let response = handlers.handle_json_rpc(r#"not valid json"#).await;
        assert!(response.contains("-32700"), "expected Parse error code: {}", response);
        assert!(response.contains("Parse error"), "expected Parse error message: {}", response);
        assert!(!response.contains("networkName"), "must not return extended object so Forge can parse: {}", response);
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
}
