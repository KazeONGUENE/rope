//! Sandboxed Execution Environment
//!
//! Provides secure execution with capability-based permissions

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::time::Duration;
use thiserror::Error;

/// Capability for sandboxed execution
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Capability {
    /// Network access
    Network,

    /// File system read
    FileRead,

    /// File system write
    FileWrite,

    /// Shell execution
    Shell,

    /// Environment variables
    Environment,

    /// Process spawn
    ProcessSpawn,

    /// Specific URL access
    UrlAccess(String),

    /// Specific file path access
    PathAccess(String),
}

/// Sandbox configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Allowed capabilities
    pub capabilities: Vec<Capability>,

    /// Maximum execution time (ms)
    pub max_execution_time_ms: u64,

    /// Maximum memory (bytes)
    pub max_memory_bytes: u64,

    /// Allowed network hosts
    pub allowed_hosts: Vec<String>,

    /// Allowed file paths
    pub allowed_paths: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            capabilities: vec![Capability::Network],
            max_execution_time_ms: 30_000,
            max_memory_bytes: 512 * 1024 * 1024, // 512 MB
            allowed_hosts: vec![
                "api.openai.com".to_string(),
                "api.anthropic.com".to_string(),
                "erpc.datachain.network".to_string(),
                "ws.datachain.network".to_string(),
            ],
            allowed_paths: vec![],
        }
    }
}

/// Sandboxed execution environment
pub struct SandboxedExecutor {
    /// Allowed capabilities
    allowed_capabilities: HashSet<Capability>,

    /// Maximum execution time
    max_execution_time: Duration,

    /// Maximum memory usage
    max_memory_bytes: u64,

    /// Allowed network hosts
    allowed_hosts: HashSet<String>,

    /// Allowed file paths
    allowed_paths: HashSet<String>,
}

impl SandboxedExecutor {
    /// Create new sandboxed executor
    pub fn new(config: SandboxConfig) -> Self {
        Self {
            allowed_capabilities: config.capabilities.into_iter().collect(),
            max_execution_time: Duration::from_millis(config.max_execution_time_ms),
            max_memory_bytes: config.max_memory_bytes,
            allowed_hosts: config.allowed_hosts.into_iter().collect(),
            allowed_paths: config.allowed_paths.into_iter().collect(),
        }
    }

    /// Check if capability is allowed
    pub fn has_capability(&self, cap: &Capability) -> bool {
        self.allowed_capabilities.contains(cap)
    }

    /// Validate network request
    pub fn validate_network_request(&self, url: &str) -> Result<(), SandboxError> {
        if !self.has_capability(&Capability::Network) {
            return Err(SandboxError::CapabilityDenied(Capability::Network));
        }

        // Parse URL to extract host
        let host = extract_host(url).ok_or_else(|| SandboxError::InvalidUrl(url.to_string()))?;

        // Check if host is allowed
        if !self.allowed_hosts.contains(&host)
            && !self.allowed_hosts.contains("*")
            && !self
                .allowed_capabilities
                .contains(&Capability::UrlAccess(url.to_string()))
        {
            return Err(SandboxError::HostNotAllowed(host));
        }

        Ok(())
    }

    /// Validate file access
    pub fn validate_file_access(&self, path: &str, write: bool) -> Result<(), SandboxError> {
        let required_cap = if write {
            Capability::FileWrite
        } else {
            Capability::FileRead
        };

        if !self.has_capability(&required_cap) {
            return Err(SandboxError::CapabilityDenied(required_cap));
        }

        // Check if path is allowed
        let normalized = normalize_path(path);

        let allowed = self
            .allowed_paths
            .iter()
            .any(|allowed| normalized.starts_with(allowed))
            || self
                .allowed_capabilities
                .contains(&Capability::PathAccess(normalized.clone()));

        if !allowed {
            return Err(SandboxError::PathNotAllowed(path.to_string()));
        }

        Ok(())
    }

    /// Execute with timeout
    pub async fn execute_with_timeout<F, T, E>(&self, f: F) -> Result<T, SandboxError>
    where
        F: std::future::Future<Output = Result<T, E>> + Send,
        E: std::fmt::Display,
    {
        match tokio::time::timeout(self.max_execution_time, f).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(SandboxError::ExecutionError(e.to_string())),
            Err(_) => Err(SandboxError::ExecutionTimeout),
        }
    }

    /// Get max execution time
    pub fn max_execution_time(&self) -> Duration {
        self.max_execution_time
    }

    /// Get max memory bytes
    pub fn max_memory_bytes(&self) -> u64 {
        self.max_memory_bytes
    }

    /// Add capability at runtime
    pub fn add_capability(&mut self, cap: Capability) {
        self.allowed_capabilities.insert(cap);
    }

    /// Add allowed host
    pub fn add_allowed_host(&mut self, host: String) {
        self.allowed_hosts.insert(host);
    }

    /// Add allowed path
    pub fn add_allowed_path(&mut self, path: String) {
        self.allowed_paths.insert(path);
    }
}

/// Extract host from URL
fn extract_host(url: &str) -> Option<String> {
    let url = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("wss://"))
        .or_else(|| url.strip_prefix("ws://"))?;

    let host = url.split('/').next()?;
    let host = host.split(':').next()?;

    if host.is_empty() {
        None
    } else {
        Some(host.to_string())
    }
}

/// Normalize file path
fn normalize_path(path: &str) -> String {
    // Simple normalization - in production, use proper path canonicalization
    path.replace("//", "/")
        .replace("/./", "/")
        .trim_end_matches('/')
        .to_string()
}

/// Sandbox errors
#[derive(Debug, Error)]
pub enum SandboxError {
    #[error("Capability denied: {0:?}")]
    CapabilityDenied(Capability),

    #[error("Host not allowed: {0}")]
    HostNotAllowed(String),

    #[error("Path not allowed: {0}")]
    PathNotAllowed(String),

    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Invalid path: {0}")]
    InvalidPath(String),

    #[error("Execution timeout")]
    ExecutionTimeout,

    #[error("Memory limit exceeded")]
    MemoryLimitExceeded,

    #[error("Execution error: {0}")]
    ExecutionError(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capability_check() {
        let config = SandboxConfig {
            capabilities: vec![Capability::Network, Capability::FileRead],
            ..Default::default()
        };

        let executor = SandboxedExecutor::new(config);

        assert!(executor.has_capability(&Capability::Network));
        assert!(executor.has_capability(&Capability::FileRead));
        assert!(!executor.has_capability(&Capability::FileWrite));
        assert!(!executor.has_capability(&Capability::Shell));
    }

    #[test]
    fn test_network_validation() {
        let config = SandboxConfig::default();
        let executor = SandboxedExecutor::new(config);

        // Allowed host
        assert!(executor
            .validate_network_request("https://api.openai.com/v1/chat")
            .is_ok());

        // Not allowed host
        assert!(executor
            .validate_network_request("https://malicious.example.com")
            .is_err());
    }

    #[test]
    fn test_extract_host() {
        assert_eq!(
            extract_host("https://api.openai.com/v1/chat"),
            Some("api.openai.com".to_string())
        );
        assert_eq!(
            extract_host("wss://ws.datachain.network"),
            Some("ws.datachain.network".to_string())
        );
        assert_eq!(extract_host("invalid"), None);
    }

    #[tokio::test]
    async fn test_timeout() {
        let config = SandboxConfig {
            max_execution_time_ms: 100,
            ..Default::default()
        };

        let executor = SandboxedExecutor::new(config);

        let result: Result<(), SandboxError> = executor
            .execute_with_timeout(async {
                tokio::time::sleep(Duration::from_secs(1)).await;
                Ok::<(), &str>(())
            })
            .await;

        assert!(matches!(result, Err(SandboxError::ExecutionTimeout)));
    }
}
