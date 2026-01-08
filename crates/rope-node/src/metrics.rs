//! Prometheus metrics server

use crate::config::MetricsSettings;
use prometheus::{Encoder, TextEncoder, Registry, Counter, Gauge};
use std::net::SocketAddr;
use std::io::Write;

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
                                // Encode metrics
                                let encoder = TextEncoder::new();
                                let metric_families = registry.gather();
                                let mut buffer = Vec::new();
                                encoder.encode(&metric_families, &mut buffer).unwrap();
                                
                                format!(
                                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                                    buffer.len(),
                                    String::from_utf8_lossy(&buffer)
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
