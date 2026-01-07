//! # RPC Server
//! 
//! gRPC API server for external clients.
//! Provides HTTP/2 + mTLS with JWT authentication.

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// RPC server configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RpcConfig {
    /// Enable RPC server
    pub enabled: bool,
    
    /// Listen address
    pub listen_addr: String,
    
    /// Maximum concurrent connections
    pub max_connections: usize,
    
    /// Request timeout
    pub request_timeout: Duration,
    
    /// Enable TLS
    pub enable_tls: bool,
    
    /// TLS certificate path
    pub tls_cert_path: Option<String>,
    
    /// TLS key path
    pub tls_key_path: Option<String>,
    
    /// Enable JWT authentication
    pub enable_jwt: bool,
    
    /// JWT secret (for HMAC)
    pub jwt_secret: Option<String>,
    
    /// Rate limit (requests per second per IP)
    pub rate_limit: u32,
    
    /// Enable request logging
    pub enable_logging: bool,
}

impl Default for RpcConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            listen_addr: "0.0.0.0:9001".to_string(),
            max_connections: 100,
            request_timeout: Duration::from_secs(30),
            enable_tls: true,
            tls_cert_path: None,
            tls_key_path: None,
            enable_jwt: true,
            jwt_secret: None,
            rate_limit: 100,
            enable_logging: true,
        }
    }
}

/// API response wrapper
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApiResponse<T> {
    pub success: bool,
    pub data: Option<T>,
    pub error: Option<String>,
    pub timestamp: i64,
}

impl<T> ApiResponse<T> {
    /// Create success response
    pub fn success(data: T) -> Self {
        Self {
            success: true,
            data: Some(data),
            error: None,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
    
    /// Create error response
    pub fn error(message: String) -> Self {
        Self {
            success: false,
            data: None,
            error: Some(message),
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
}

/// API endpoints
pub mod endpoints {
    //! RPC endpoint definitions
    
    /// String operations
    pub const STRING_GET: &str = "/v1/string/get";
    pub const STRING_CREATE: &str = "/v1/string/create";
    pub const STRING_LIST: &str = "/v1/string/list";
    
    /// Consensus operations
    pub const CONSENSUS_STATUS: &str = "/v1/consensus/status";
    pub const CONSENSUS_TESTIMONIES: &str = "/v1/consensus/testimonies";
    
    /// Network operations
    pub const NETWORK_PEERS: &str = "/v1/network/peers";
    pub const NETWORK_STATS: &str = "/v1/network/stats";
    
    /// Token operations
    pub const TOKEN_BALANCE: &str = "/v1/token/balance";
    pub const TOKEN_TRANSFER: &str = "/v1/token/transfer";
    pub const TOKEN_MINT: &str = "/v1/token/mint";
    
    /// Health check
    pub const HEALTH: &str = "/health";
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_rpc_config() {
        let config = RpcConfig::default();
        assert!(config.enabled);
        assert!(config.enable_tls);
        assert_eq!(config.rate_limit, 100);
    }
    
    #[test]
    fn test_api_response() {
        let response: ApiResponse<String> = ApiResponse::success("test".to_string());
        assert!(response.success);
        assert_eq!(response.data, Some("test".to_string()));
        
        let error: ApiResponse<String> = ApiResponse::error("failed".to_string());
        assert!(!error.success);
        assert_eq!(error.error, Some("failed".to_string()));
    }
}

