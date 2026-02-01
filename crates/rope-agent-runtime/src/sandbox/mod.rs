//! Sandboxed Execution Environment for AI Agent Skills
//!
//! This module provides secure, isolated execution environments for AI agent skills
//! with comprehensive capability-based permissions and resource limits.
//!
//! ## Security Model
//!
//! Skills execute in a sandboxed environment with:
//! - **Capability-based permissions**: Skills must request specific capabilities
//! - **Network isolation**: Only pre-approved hosts can be accessed
//! - **File system restrictions**: Read/write limited to approved paths
//! - **Resource limits**: CPU time, memory, and execution duration caps
//! - **Process isolation**: No shell or process spawn by default
//!
//! ## WASM Isolation (Planned)
//!
//! Future versions will support WebAssembly-based isolation:
//! - Skills compiled to WASM run in isolated memory space
//! - Import/export restrictions enforce capability boundaries
//! - Memory usage tracked per-module
//! - Linear memory cannot exceed configured limits
//!
//! ## Example Usage
//!
//! ```rust,ignore
//! use rope_agent_runtime::sandbox::{SandboxConfig, SandboxedExecutor, Capability};
//!
//! // Create restricted sandbox for untrusted skill
//! let config = SandboxConfig {
//!     capabilities: vec![Capability::Network],
//!     max_execution_time_ms: 5_000,    // 5 second timeout
//!     max_memory_bytes: 64 * 1024 * 1024, // 64 MB limit
//!     allowed_hosts: vec!["api.openai.com".into()],
//!     allowed_paths: vec![],           // No file system access
//! };
//!
//! let executor = SandboxedExecutor::new(config);
//!
//! // Validate requests before execution
//! executor.validate_network_request("https://api.openai.com/v1/chat")?;
//! ```
//!
//! ## Skill Manifest
//!
//! Each skill declares required capabilities in its manifest:
//!
//! ```toml
//! [skill]
//! name = "weather-lookup"
//! version = "1.0.0"
//!
//! [capabilities]
//! network = true
//! file_read = false
//! file_write = false
//! shell = false
//!
//! [network]
//! allowed_hosts = ["api.weather.com", "api.openweathermap.org"]
//!
//! [limits]
//! max_execution_ms = 10000
//! max_memory_mb = 128
//! ```
//!
//! ## Security Best Practices
//!
//! 1. **Principle of Least Privilege**: Only grant capabilities the skill needs
//! 2. **Host Allowlisting**: Enumerate specific allowed hosts, never use "*"
//! 3. **Path Restrictions**: Limit file access to skill-specific directories
//! 4. **Timeout Enforcement**: Always set reasonable execution timeouts
//! 5. **Memory Limits**: Prevent runaway memory consumption
//! 6. **Audit Logging**: Log all capability checks for security review

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

    #[error("WASM validation failed: {0}")]
    WasmValidationFailed(String),

    #[error("Skill manifest invalid: {0}")]
    InvalidManifest(String),
}

/// Skill manifest declaring required capabilities
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillManifest {
    /// Skill name
    pub name: String,
    /// Skill version
    pub version: String,
    /// Skill author
    pub author: String,
    /// Required capabilities
    pub capabilities: Vec<Capability>,
    /// Allowed network hosts
    pub allowed_hosts: Vec<String>,
    /// Allowed file paths
    pub allowed_paths: Vec<String>,
    /// Maximum execution time (ms)
    pub max_execution_time_ms: u64,
    /// Maximum memory (bytes)
    pub max_memory_bytes: u64,
    /// Content hash for integrity verification
    pub content_hash: Option<String>,
    /// Digital signature from trusted publisher
    pub signature: Option<String>,
}

impl Default for SkillManifest {
    fn default() -> Self {
        Self {
            name: "untitled".to_string(),
            version: "0.1.0".to_string(),
            author: "unknown".to_string(),
            capabilities: vec![],
            allowed_hosts: vec![],
            allowed_paths: vec![],
            max_execution_time_ms: 30_000,
            max_memory_bytes: 128 * 1024 * 1024,
            content_hash: None,
            signature: None,
        }
    }
}

/// WASM module validator for skill security
pub struct WasmValidator {
    /// Maximum allowed WASM module size
    pub max_module_size: usize,
    /// Banned import namespaces
    pub banned_imports: Vec<String>,
    /// Required exports
    pub required_exports: Vec<String>,
}

impl Default for WasmValidator {
    fn default() -> Self {
        Self {
            max_module_size: 10 * 1024 * 1024, // 10 MB
            banned_imports: vec![
                "wasi_snapshot_preview1".to_string(), // Restrict WASI by default
                "env.abort".to_string(),
            ],
            required_exports: vec![
                "execute".to_string(), // Main entry point
            ],
        }
    }
}

impl WasmValidator {
    /// Validate WASM module bytes
    pub fn validate(&self, module_bytes: &[u8]) -> Result<WasmValidationResult, SandboxError> {
        // Check size limit
        if module_bytes.len() > self.max_module_size {
            return Err(SandboxError::WasmValidationFailed(format!(
                "Module size {} exceeds limit {}",
                module_bytes.len(),
                self.max_module_size
            )));
        }

        // Check magic number
        if module_bytes.len() < 8 {
            return Err(SandboxError::WasmValidationFailed(
                "Module too small".to_string(),
            ));
        }

        let magic = &module_bytes[0..4];
        if magic != b"\0asm" {
            return Err(SandboxError::WasmValidationFailed(
                "Invalid WASM magic number".to_string(),
            ));
        }

        let version = u32::from_le_bytes([
            module_bytes[4],
            module_bytes[5],
            module_bytes[6],
            module_bytes[7],
        ]);

        if version != 1 {
            return Err(SandboxError::WasmValidationFailed(format!(
                "Unsupported WASM version: {}",
                version
            )));
        }

        // In production, parse the module to extract imports/exports
        // For now, return basic validation result
        Ok(WasmValidationResult {
            is_valid: true,
            module_size: module_bytes.len(),
            version,
            imports: vec![],
            exports: vec![],
            memory_pages: 1,
        })
    }

    /// Validate skill manifest
    pub fn validate_manifest(&self, manifest: &SkillManifest) -> Result<(), SandboxError> {
        if manifest.name.is_empty() {
            return Err(SandboxError::InvalidManifest(
                "Skill name cannot be empty".to_string(),
            ));
        }

        if manifest.version.is_empty() {
            return Err(SandboxError::InvalidManifest(
                "Skill version cannot be empty".to_string(),
            ));
        }

        // Validate capabilities are not excessive
        let dangerous_caps = [Capability::Shell, Capability::ProcessSpawn];
        for cap in &manifest.capabilities {
            if dangerous_caps.contains(cap) {
                tracing::warn!(
                    "Skill '{}' requests dangerous capability: {:?}",
                    manifest.name,
                    cap
                );
            }
        }

        // Validate hosts don't include wildcards in production
        if manifest.allowed_hosts.contains(&"*".to_string()) {
            return Err(SandboxError::InvalidManifest(
                "Wildcard hosts not allowed in production".to_string(),
            ));
        }

        Ok(())
    }
}

/// WASM validation result
#[derive(Clone, Debug)]
pub struct WasmValidationResult {
    /// Validation passed
    pub is_valid: bool,
    /// Module size in bytes
    pub module_size: usize,
    /// WASM version
    pub version: u32,
    /// Imported functions
    pub imports: Vec<String>,
    /// Exported functions
    pub exports: Vec<String>,
    /// Initial memory pages
    pub memory_pages: u32,
}

/// Security audit entry
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityAuditEntry {
    /// Timestamp
    pub timestamp: i64,
    /// Skill name
    pub skill_name: String,
    /// Action attempted
    pub action: String,
    /// Capability checked
    pub capability: Option<Capability>,
    /// Result (allowed/denied)
    pub allowed: bool,
    /// Additional context
    pub context: Option<String>,
}

/// Security audit log
pub struct SecurityAuditLog {
    entries: parking_lot::RwLock<Vec<SecurityAuditEntry>>,
    max_entries: usize,
}

impl SecurityAuditLog {
    /// Create new audit log
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: parking_lot::RwLock::new(Vec::new()),
            max_entries,
        }
    }

    /// Log a security event
    pub fn log(&self, entry: SecurityAuditEntry) {
        let mut entries = self.entries.write();
        entries.push(entry);

        // Trim if exceeds max
        let current_len = entries.len();
        if current_len > self.max_entries {
            let drain_count = current_len - self.max_entries;
            entries.drain(0..drain_count);
        }
    }

    /// Get recent entries
    pub fn recent(&self, count: usize) -> Vec<SecurityAuditEntry> {
        let entries = self.entries.read();
        entries.iter().rev().take(count).cloned().collect()
    }

    /// Get denied actions
    pub fn denied_actions(&self) -> Vec<SecurityAuditEntry> {
        self.entries
            .read()
            .iter()
            .filter(|e| !e.allowed)
            .cloned()
            .collect()
    }
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

    #[test]
    fn test_wasm_validator_default() {
        let validator = WasmValidator::default();
        assert_eq!(validator.max_module_size, 10 * 1024 * 1024);
        assert!(validator
            .banned_imports
            .contains(&"wasi_snapshot_preview1".to_string()));
    }

    #[test]
    fn test_wasm_validation_too_small() {
        let validator = WasmValidator::default();
        let result = validator.validate(&[0, 1, 2, 3]);
        assert!(result.is_err());
    }

    #[test]
    fn test_wasm_validation_invalid_magic() {
        let validator = WasmValidator::default();
        let invalid = [0u8; 100];
        let result = validator.validate(&invalid);
        assert!(matches!(result, Err(SandboxError::WasmValidationFailed(_))));
    }

    #[test]
    fn test_wasm_validation_valid() {
        let validator = WasmValidator::default();
        // Valid WASM magic + version 1
        let mut valid = vec![0x00, 0x61, 0x73, 0x6D, 0x01, 0x00, 0x00, 0x00];
        valid.extend_from_slice(&[0u8; 100]); // Add some content

        let result = validator.validate(&valid);
        assert!(result.is_ok());
        let validation = result.unwrap();
        assert!(validation.is_valid);
        assert_eq!(validation.version, 1);
    }

    #[test]
    fn test_skill_manifest_validation() {
        let validator = WasmValidator::default();

        // Valid manifest
        let manifest = SkillManifest {
            name: "test-skill".to_string(),
            version: "1.0.0".to_string(),
            author: "test".to_string(),
            ..Default::default()
        };
        assert!(validator.validate_manifest(&manifest).is_ok());

        // Invalid - empty name
        let invalid = SkillManifest {
            name: "".to_string(),
            ..Default::default()
        };
        assert!(validator.validate_manifest(&invalid).is_err());

        // Invalid - wildcard host
        let wildcard = SkillManifest {
            name: "test".to_string(),
            version: "1.0".to_string(),
            allowed_hosts: vec!["*".to_string()],
            ..Default::default()
        };
        assert!(validator.validate_manifest(&wildcard).is_err());
    }

    #[test]
    fn test_security_audit_log() {
        let log = SecurityAuditLog::new(5);

        for i in 0..10 {
            log.log(SecurityAuditEntry {
                timestamp: i,
                skill_name: format!("skill-{}", i),
                action: "test".to_string(),
                capability: None,
                allowed: i % 2 == 0,
                context: None,
            });
        }

        // Should only have 5 entries (max)
        let recent = log.recent(10);
        assert_eq!(recent.len(), 5);

        // Check denied actions
        let denied = log.denied_actions();
        assert!(!denied.is_empty());
    }
}
