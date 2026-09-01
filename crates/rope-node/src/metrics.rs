//! Prometheus metrics server
//!
//! Registry gauges (`rope_strings_total`, …) stay for future wiring. Scrape-time
//! process samples come from `/proc/self` — we do **not** use jemalloc
//! (`tikv-jemallocator` is not the production allocator). DashMap shard
//! `try_write` sampling is intentionally omitted: it needs an `Arc` into
//! `StringLattice` and is not required for the P1.4 soak (use
//! `deploy/scripts/p14-soak-monitor.sh` + hang dumps instead).

use crate::config::MetricsSettings;
use prometheus::{Counter, Encoder, Gauge, Registry, TextEncoder};
use std::io::Write;
use std::net::SocketAddr;

/// Metrics server
pub struct MetricsServer {
    /// Configuration
    config: MetricsSettings,
    /// Prometheus registry
    registry: Registry,
}

impl MetricsServer {
    /// Create new metrics server
    pub fn new(config: &MetricsSettings) -> anyhow::Result<Self> {
        let registry = Registry::new();

        // Register default metrics
        let strings_total = Counter::new("rope_strings_total", "Total strings in lattice")?;
        let transactions_total = Counter::new("rope_transactions_total", "Total transactions")?;
        let peers_connected = Gauge::new("rope_peers_connected", "Connected peers")?;
        let block_height = Gauge::new("rope_block_height", "Current block height")?;
        let ai_agents_active = Gauge::new("rope_ai_agents_active", "Active AI testimony agents")?;

        registry.register(Box::new(strings_total))?;
        registry.register(Box::new(transactions_total))?;
        registry.register(Box::new(peers_connected))?;
        registry.register(Box::new(block_height))?;
        registry.register(Box::new(ai_agents_active))?;

        Ok(Self {
            config: config.clone(),
            registry,
        })
    }

    /// Run the metrics server
    pub async fn run(&self) -> anyhow::Result<()> {
        let addr: SocketAddr = self.config.prometheus_addr.parse()?;

        tracing::info!("Starting metrics server on {}", addr);

        // Use standard TCP listener
        let listener = std::net::TcpListener::bind(addr)?;
        listener.set_nonblocking(true)?;

        loop {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let registry = self.registry.clone();

                    // Handle request synchronously in a blocking task
                    tokio::task::spawn_blocking(move || {
                        let mut buf = [0u8; 1024];
                        if let Ok(n) = std::io::Read::read(&mut stream, &mut buf) {
                            let request = String::from_utf8_lossy(&buf[..n]);

                            let response = if request.contains("GET /metrics") {
                                let body = encode_metrics_body(&registry);
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n{}",
                                    body.len(),
                                    body
                                )
                            } else if request.contains("GET /health") {
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"status\":\"healthy\"}".to_string()
                            } else {
                                "HTTP/1.1 404 Not Found\r\n\r\n".to_string()
                            };

                            let _ = stream.write_all(response.as_bytes());
                        }
                    });
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    // No incoming connections, sleep briefly
                    tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                }
                Err(e) => {
                    tracing::error!("Accept error: {}", e);
                }
            }
        }
    }
}

fn encode_metrics_body(registry: &Registry) -> String {
    let encoder = TextEncoder::new();
    let metric_families = registry.gather();
    let mut buffer = Vec::new();
    if encoder.encode(&metric_families, &mut buffer).is_err() {
        return String::new();
    }
    let mut body = String::from_utf8_lossy(&buffer).into_owned();
    body.push_str(&process_proc_metrics());
    body
}

/// Live process footprint from `/proc/self` (Linux). Zero-overhead when not
/// scraped; no allocator-specific deps.
fn process_proc_metrics() -> String {
    #[cfg(target_os = "linux")]
    {
        let status = match std::fs::read_to_string("/proc/self/status") {
            Ok(s) => s,
            Err(_) => return String::new(),
        };
        let mut rss_kb: u64 = 0;
        let mut threads: u64 = 0;
        let mut vsize_kb: u64 = 0;
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                rss_kb = parse_kb_field(rest);
            } else if let Some(rest) = line.strip_prefix("VmSize:") {
                vsize_kb = parse_kb_field(rest);
            } else if let Some(rest) = line.strip_prefix("Threads:") {
                threads = rest.trim().parse().unwrap_or(0);
            }
        }
        format!(
            "\n\
             # HELP process_resident_memory_bytes Resident set size from /proc/self/status VmRSS\n\
             # TYPE process_resident_memory_bytes gauge\n\
             process_resident_memory_bytes {}\n\
             # HELP process_virtual_memory_bytes Virtual memory size from /proc/self/status VmSize\n\
             # TYPE process_virtual_memory_bytes gauge\n\
             process_virtual_memory_bytes {}\n\
             # HELP process_threads OS thread count from /proc/self/status\n\
             # TYPE process_threads gauge\n\
             process_threads {}\n",
            rss_kb.saturating_mul(1024),
            vsize_kb.saturating_mul(1024),
            threads
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        String::new()
    }
}

#[cfg(target_os = "linux")]
fn parse_kb_field(rest: &str) -> u64 {
    rest.split_whitespace()
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_metrics_body_includes_registry_families() {
        let registry = Registry::new();
        let g = Gauge::new("rope_test_gauge", "test").unwrap();
        g.set(42.0);
        registry.register(Box::new(g)).unwrap();
        let body = encode_metrics_body(&registry);
        assert!(body.contains("rope_test_gauge"));
        assert!(body.contains("42"));
    }
}
