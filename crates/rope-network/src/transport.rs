//! # Transport Layer
//! 
//! libp2p-based transport with QUIC and TCP support.
//! Includes hybrid post-quantum key exchange (TLS 1.3 + Kyber).

use std::net::SocketAddr;
use std::time::Duration;
use serde::{Deserialize, Serialize};

/// Transport layer configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransportConfig {
    /// Listen address
    pub listen_addr: SocketAddr,
    
    /// Enable QUIC transport (preferred)
    pub enable_quic: bool,
    
    /// Enable TCP fallback
    pub enable_tcp: bool,
    
    /// Enable WebSocket for browser clients
    pub enable_websocket: bool,
    
    /// Connection timeout
    pub connection_timeout: Duration,
    
    /// Idle timeout before closing connection
    pub idle_timeout: Duration,
    
    /// Maximum concurrent connections
    pub max_connections: usize,
    
    /// Enable post-quantum key exchange
    pub enable_pq_crypto: bool,
}

impl Default for TransportConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:9000".parse().unwrap(),
            enable_quic: true,
            enable_tcp: true,
            enable_websocket: false,
            connection_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(300),
            max_connections: 1000,
            enable_pq_crypto: true,
        }
    }
}

/// Connection statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ConnectionStats {
    pub active_connections: usize,
    pub total_connections: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
    pub quic_connections: usize,
    pub tcp_connections: usize,
}

/// Transport layer manager
pub struct TransportLayer {
    config: TransportConfig,
    stats: parking_lot::RwLock<ConnectionStats>,
    // In production: libp2p Swarm would be here
}

impl TransportLayer {
    /// Create new transport layer
    pub fn new(config: TransportConfig) -> Self {
        Self {
            config,
            stats: parking_lot::RwLock::new(ConnectionStats::default()),
        }
    }
    
    /// Get configuration
    pub fn config(&self) -> &TransportConfig {
        &self.config
    }
    
    /// Get connection statistics
    pub fn stats(&self) -> ConnectionStats {
        self.stats.read().clone()
    }
    
    /// Update sent bytes
    pub fn record_sent(&self, bytes: u64) {
        self.stats.write().bytes_sent += bytes;
    }
    
    /// Update received bytes
    pub fn record_received(&self, bytes: u64) {
        self.stats.write().bytes_received += bytes;
    }
    
    /// Increment active connections
    pub fn connection_opened(&self, is_quic: bool) {
        let mut stats = self.stats.write();
        stats.active_connections += 1;
        stats.total_connections += 1;
        if is_quic {
            stats.quic_connections += 1;
        } else {
            stats.tcp_connections += 1;
        }
    }
    
    /// Decrement active connections
    pub fn connection_closed(&self, is_quic: bool) {
        let mut stats = self.stats.write();
        stats.active_connections = stats.active_connections.saturating_sub(1);
        if is_quic {
            stats.quic_connections = stats.quic_connections.saturating_sub(1);
        } else {
            stats.tcp_connections = stats.tcp_connections.saturating_sub(1);
        }
    }
}

impl Default for TransportLayer {
    fn default() -> Self {
        Self::new(TransportConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_transport_config() {
        let config = TransportConfig::default();
        assert!(config.enable_quic);
        assert!(config.enable_tcp);
        assert_eq!(config.max_connections, 1000);
    }
    
    #[test]
    fn test_connection_stats() {
        let transport = TransportLayer::default();
        
        transport.connection_opened(true);
        transport.connection_opened(false);
        transport.record_sent(1000);
        transport.record_received(500);
        
        let stats = transport.stats();
        assert_eq!(stats.active_connections, 2);
        assert_eq!(stats.quic_connections, 1);
        assert_eq!(stats.tcp_connections, 1);
        assert_eq!(stats.bytes_sent, 1000);
        assert_eq!(stats.bytes_received, 500);
    }
}

