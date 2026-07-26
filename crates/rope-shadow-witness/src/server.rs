//! JSON-RPC server exposing the v2 advisory methods.
//!
//! Two methods, both prefixed `rope_v2_` to mark their advisory status
//! and to leave the canonical `rope_*` namespace untouched:
//!
//! - `rope_v2_knotHash(string_id, event_id)` returns the v2 chain
//!   entry for one (string, event_id) pair.
//! - `rope_v2_walkChain(string_id, offset, limit)` returns a window of
//!   the v2 chain for one string.
//!
//! A small `rope_v2_status` method is also exposed for liveness probes
//! and operational dashboards.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;

use serde_json::{json, Value};
use tracing::{debug, info};

use crate::chain::ShadowChain;
use crate::config::ShadowWitnessConfig;
use crate::error::{ShadowWitnessError, ShadowWitnessResult};
use crate::store::parse_string_id_hex;

const RPC_PARSE_ERROR: i64 = -32700;
const RPC_INVALID_REQUEST: i64 = -32600;
const RPC_METHOD_NOT_FOUND: i64 = -32601;
const RPC_INVALID_PARAMS: i64 = -32602;
const RPC_INTERNAL: i64 = -32603;

pub struct Server {
    chain: Arc<ShadowChain>,
    config: ShadowWitnessConfig,
}

impl Server {
    pub fn new(chain: Arc<ShadowChain>, config: ShadowWitnessConfig) -> Self {
        Self { chain, config }
    }

    /// Start the HTTP listener using a hyper-style minimal handler.
    /// Blocks on the runtime; intended to be spawned on a tokio task.
    pub async fn serve(self: Arc<Self>) -> ShadowWitnessResult<()> {
        let addr: SocketAddr = self
            .config
            .bind_addr
            .parse()
            .map_err(|e| ShadowWitnessError::Bind(format!("invalid bind addr: {}", e)))?;

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|e| ShadowWitnessError::Bind(format!("bind {}: {}", addr, e)))?;
        info!(bind = %addr, "shadow witness: rpc listening");

        loop {
            let (mut stream, peer) = match listener.accept().await {
                Ok(p) => p,
                Err(e) => {
                    debug!(error = %e, "shadow witness: accept failed");
                    continue;
                }
            };
            let me = self.clone();
            tokio::spawn(async move {
                if let Err(e) = handle_connection(me, &mut stream).await {
                    debug!(peer = %peer, error = %e, "shadow witness: connection handler failed");
                }
            });
        }
    }

    fn dispatch(&self, method: &str, params: &Value) -> Result<Value, JsonRpcError> {
        match method {
            "rope_v2_status" => {
                let store = self.chain.store();
                let observed_strings = store
                    .total_strings()
                    .map_err(|e| internal(e.to_string()))?;
                let observed_knots = store
                    .total_entries()
                    .map_err(|e| internal(e.to_string()))?;
                let last_observed_at_unix = store
                    .last_observed_at_unix()
                    .map_err(|e| internal(e.to_string()))?;
                let first_observed_at_unix = store
                    .first_observed_at_unix()
                    .map_err(|e| internal(e.to_string()))?;
                // Read the immutable install marker written by main.rs at
                // first start-up. This is the soak gate's anchor (it does
                // NOT drift forward as heads are refreshed). 0 if missing.
                let install_marker = self.config.data_dir.join(".first-install-at-unix");
                let first_install_at_unix: i64 = std::fs::read_to_string(&install_marker)
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .unwrap_or(0);
                Ok(json!({
                    "version": "0.1.2",
                    "spec": "Quipu Primitive Canon §6.1.1 (v2 knot-hash construction)",
                    "witness_tag": self.config.witness_tag,
                    "upstream": self.config.upstream_rpc_url,
                    "fidelity": "v0.1 — RPC-poll mode; metadata fields populated via observable proxies; chain-continuity-under-erasure verified",
                    "observed_strings": observed_strings,
                    "observed_knots": observed_knots,
                    "first_install_at_unix": first_install_at_unix,
                    "first_observed_at_unix": first_observed_at_unix,
                    "last_observed_at_unix": last_observed_at_unix,
                }))
            }
            "rope_v2_knotHash" => {
                let string_id_hex = params
                    .get(0)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid("expected params[0] = string_id hex"))?;
                let event_id = params
                    .get(1)
                    .and_then(|v| v.as_u64())
                    .ok_or_else(|| invalid("expected params[1] = event_id u64"))?;
                let id_bytes = parse_string_id_hex(string_id_hex).map_err(|e| invalid(e.to_string()))?;
                match self
                    .chain
                    .store()
                    .get_entry(&id_bytes, event_id)
                    .map_err(|e| internal(e.to_string()))?
                {
                    None => Ok(json!(null)),
                    Some(entry) => Ok(json!({
                        "string_id": entry.string_id,
                        "event_id": entry.event_id,
                        "event_type": entry.event_type,
                        "is_tombstone": entry.is_tombstone,
                        "knot_hash": format!("0x{}", entry.knot_hash.to_hex()),
                        "previous_hash": format!("0x{}", entry.previous_hash.to_hex()),
                        "event_metadata_hash": format!("0x{}", entry.event_metadata_hash.to_hex()),
                        "observed_at_unix": entry.observed_at_unix,
                    })),
                }
            }
            "rope_v2_walkChain" => {
                let string_id_hex = params
                    .get(0)
                    .and_then(|v| v.as_str())
                    .ok_or_else(|| invalid("expected params[0] = string_id hex"))?;
                let offset = params
                    .get(1)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as usize;
                let limit = params
                    .get(2)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(50)
                    .min(500) as usize;
                let id_bytes = parse_string_id_hex(string_id_hex).map_err(|e| invalid(e.to_string()))?;
                let entries = self
                    .chain
                    .store()
                    .walk_chain(&id_bytes, offset, limit)
                    .map_err(|e| internal(e.to_string()))?;
                let arr: Vec<Value> = entries
                    .iter()
                    .map(|e| {
                        json!({
                            "event_id": e.event_id,
                            "event_type": e.event_type,
                            "is_tombstone": e.is_tombstone,
                            "knot_hash": format!("0x{}", e.knot_hash.to_hex()),
                            "previous_hash": format!("0x{}", e.previous_hash.to_hex()),
                            "event_metadata_hash": format!("0x{}", e.event_metadata_hash.to_hex()),
                            "observed_at_unix": e.observed_at_unix,
                        })
                    })
                    .collect();
                let head = self
                    .chain
                    .store()
                    .get_head(&id_bytes)
                    .map_err(|e| internal(e.to_string()))?;
                let head_value = match head {
                    None => Value::Null,
                    Some(h) => json!({
                        "latest_event_id": h.latest_event_id,
                        "latest_knot_hash": format!("0x{}", h.latest_knot_hash.to_hex()),
                        "updated_at_unix": h.updated_at_unix,
                    }),
                };
                Ok(json!({
                    "string_id": string_id_hex,
                    "offset": offset,
                    "limit": limit,
                    "entries": arr,
                    "head": head_value,
                }))
            }
            _ => Err(JsonRpcError {
                code: RPC_METHOD_NOT_FOUND,
                message: format!("method not found: {}", method),
            }),
        }
    }
}

async fn handle_connection(server: Arc<Server>, stream: &mut tokio::net::TcpStream) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mut buf = vec![0u8; 65536];
    let n = stream.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let body_offset = match find_body_offset(&buf[..n]) {
        Some(o) => o,
        None => {
            let resp = build_http_response(400, b"missing body separator\n");
            stream.write_all(&resp).await?;
            return Ok(());
        }
    };

    let raw = &buf[body_offset..n];
    let request: Value = match serde_json::from_slice(raw) {
        Ok(v) => v,
        Err(e) => {
            let err = json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": { "code": RPC_PARSE_ERROR, "message": format!("parse: {}", e) }
            });
            let body = serde_json::to_vec(&err)?;
            stream.write_all(&build_http_response(200, &body)).await?;
            return Ok(());
        }
    };

    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
    let params = request.get("params").cloned().unwrap_or(json!([]));

    if method.is_empty() {
        let err = json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": RPC_INVALID_REQUEST, "message": "missing method" }
        });
        let body = serde_json::to_vec(&err)?;
        stream.write_all(&build_http_response(200, &body)).await?;
        return Ok(());
    }

    let response = match server.dispatch(method, &params) {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(e) => json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": e.code, "message": e.message }
        }),
    };

    let body = serde_json::to_vec(&response)?;
    stream.write_all(&build_http_response(200, &body)).await?;
    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
}

fn find_body_offset(buf: &[u8]) -> Option<usize> {
    for i in 0..buf.len().saturating_sub(3) {
        if &buf[i..i + 4] == b"\r\n\r\n" {
            return Some(i + 4);
        }
    }
    None
}

fn build_http_response(status: u16, body: &[u8]) -> Vec<u8> {
    let status_text = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "OK",
    };
    let mut out = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        status, status_text, body.len()
    )
    .into_bytes();
    out.extend_from_slice(body);
    out
}

struct JsonRpcError {
    code: i64,
    message: String,
}

fn invalid(msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: RPC_INVALID_PARAMS,
        message: msg.into(),
    }
}

fn internal(msg: impl Into<String>) -> JsonRpcError {
    JsonRpcError {
        code: RPC_INTERNAL,
        message: msg.into(),
    }
}

// allow cfg(test) compilation cleanly
#[allow(dead_code)]
fn _phantom_infallible() -> Infallible {
    unreachable!()
}
