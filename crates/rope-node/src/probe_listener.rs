//! Loopback-only health probe listener (2026-09-01).
//!
//! HA and nginx upstream checks must stay responsive even when the main
//! JSON-RPC Tokio pool is saturated by `rope_appendToLedger` traffic +
//! lazy-rehydrate `spawn_blocking` work. This module runs a dedicated
//! OS thread with a blocking `TcpListener` that reads the in-process
//! tip cache (`Arc<parking_lot::RwLock<u64>>`) maintained by the
//! background refresh task in `rpc_server.rs::RpcServer::run`.
//!
//! Default bind: `127.0.0.1:8544` (`ROPE_PROBE_LISTEN` override).
//! Endpoints:
//!   GET /healthz  -> {"ok":true}
//!   GET /v1/tip   -> {"ok":true,"block_hex":"0x..."}

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

/// Start the probe listener thread. Idempotent bind failure is logged and
/// ignored so production RPC keeps serving even if the port is taken.
pub fn spawn_probe_listener(tip: Arc<parking_lot::RwLock<u64>>) {
    let addr = std::env::var("ROPE_PROBE_LISTEN").unwrap_or_else(|_| "127.0.0.1:8544".to_string());
    std::thread::Builder::new()
        .name("rope-probe".into())
        .spawn(move || probe_loop(&addr, tip))
        .ok();
}

fn probe_loop(addr: &str, tip: Arc<parking_lot::RwLock<u64>>) {
    let listener = match TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(
                target: "rope_node::probe",
                addr = addr,
                error = %e,
                "probe listener bind failed; HA must fall back to JSON-RPC probe"
            );
            return;
        }
    };
    if let Err(e) = listener.set_nonblocking(false) {
        tracing::warn!(target: "rope_node::probe", error = %e, "probe listener set_nonblocking failed");
    }
    tracing::info!(
        target: "rope_node::probe",
        addr = addr,
        "loopback probe listener active (GET /v1/tip, GET /healthz)"
    );

    for stream in listener.incoming() {
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap_or(0);
        if n == 0 {
            continue;
        }
        let req = String::from_utf8_lossy(&buf[..n]);
        let (status_line, body) = if req.contains("GET /healthz") {
            ("200 OK", r#"{"ok":true}"#.to_string())
        } else if req.contains("GET /v1/tip") {
            let n = *tip.read();
            let hex = if n > 0 {
                format!("0x{n:x}")
            } else {
                "0x0".to_string()
            };
            (
                "200 OK",
                format!(r#"{{"ok":true,"block_hex":"{hex}"}}"#),
            )
        } else {
            (
                "404 Not Found",
                r#"{"ok":false,"error":"not found"}"#.to_string(),
            )
        };

        let resp = format!(
            "HTTP/1.1 {status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn probe_tip_returns_cached_hex() {
        let tip = Arc::new(parking_lot::RwLock::new(0x41u64));
        let tip_bg = tip.clone();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = [0u8; 1024];
                let n = s.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let (status, body) = if req.contains("GET /v1/tip") {
                    let n = *tip_bg.read();
                    ("200 OK", format!(r#"{{"ok":true,"block_hex":"0x{n:x}"}}"#))
                } else {
                    ("404 Not Found", r#"{"ok":false}"#.to_string())
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });

        let mut s = TcpStream::connect(&addr).unwrap();
        s.write_all(b"GET /v1/tip HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .unwrap();
        let mut out = String::new();
        s.read_to_string(&mut out).unwrap();
        assert!(out.contains(r#""block_hex":"0x41""#));
    }
}
