//! Runtime configuration

use crate::agents::PersonalCapability;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// RopeAgent runtime configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuntimeConfig {
    /// String Lattice RPC endpoints
    pub lattice_endpoints: Vec<String>,

    /// WebSocket URL for real-time updates
    pub websocket_url: String,

    /// Enabled capabilities for personal agent
    pub enabled_capabilities: Vec<PersonalCapability>,

    /// Sandbox configuration for local execution
    pub sandbox_config: SandboxConfig,

    /// Local cache directory for skills
    pub cache_dir: PathBuf,

    /// Encrypted memory storage path
    pub memory_path: PathBuf,

    /// Maximum concurrent actions
    pub max_concurrent_actions: usize,

    /// Action timeout in seconds
    pub action_timeout_secs: u64,

    /// Enable debug logging
    pub debug_mode: bool,

    /// Testimony consensus settings
    pub testimony_settings: TestimonySettings,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        let cache_dir = dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ropeagent");

        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("ropeagent");

        Self {
            lattice_endpoints: vec![
                "https://erpc.datachain.network".to_string(),
                "https://erpc.rope.network".to_string(),
            ],
            websocket_url: "wss://ws.datachain.network".to_string(),
            enabled_capabilities: vec![PersonalCapability::Messaging, PersonalCapability::Calendar],
            sandbox_config: SandboxConfig::default(),
            cache_dir,
            memory_path: data_dir.join("memory.enc"),
            max_concurrent_actions: 10,
            action_timeout_secs: 60,
            debug_mode: false,
            testimony_settings: TestimonySettings::default(),
        }
    }
}

/// Sandbox configuration for local execution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Enable network access in sandbox
    pub allow_network: bool,

    /// Enable file system access
    pub allow_filesystem: bool,

    /// Allowed file paths (if filesystem enabled)
    pub allowed_paths: Vec<PathBuf>,

    /// Maximum execution time per action (ms)
    pub max_execution_time_ms: u64,

    /// Maximum memory usage (bytes)
    pub max_memory_bytes: u64,

    /// Enable shell command execution
    pub allow_shell: bool,

    /// Whitelisted shell commands
    pub shell_whitelist: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            allow_network: true,
            allow_filesystem: false,
            allowed_paths: Vec::new(),
            max_execution_time_ms: 30_000,
            max_memory_bytes: 512 * 1024 * 1024, // 512 MB
            allow_shell: false,
            shell_whitelist: Vec::new(),
        }
    }
}

/// Testimony consensus settings
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestimonySettings {
    /// Minimum testimonies required for standard actions
    pub min_testimonies: u32,

    /// Minimum confidence score (0.0 - 1.0)
    pub min_confidence: f64,

    /// Timeout for testimony collection (seconds)
    pub timeout_secs: u64,

    /// Retry attempts for failed testimony requests
    pub retry_attempts: u32,

    /// High value threshold (USD) requiring additional testimonies
    pub high_value_threshold_usd: u64,

    /// Additional testimonies for high-value actions
    pub high_value_extra_testimonies: u32,
}

impl Default for TestimonySettings {
    fn default() -> Self {
        Self {
            min_testimonies: 3,
            min_confidence: 0.8,
            timeout_secs: 30,
            retry_attempts: 3,
            high_value_threshold_usd: 10_000,
            high_value_extra_testimonies: 2,
        }
    }
}

impl RuntimeConfig {
    /// Load configuration from file
    pub fn load(path: &std::path::Path) -> Result<Self, std::io::Error> {
        let content = std::fs::read_to_string(path)?;
        serde_json::from_str(&content)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// Save configuration to file
    pub fn save(&self, path: &std::path::Path) -> Result<(), std::io::Error> {
        let content = serde_json::to_string_pretty(self)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(path, content)
    }

    /// Create configuration for development/testing
    pub fn development() -> Self {
        Self {
            debug_mode: true,
            lattice_endpoints: vec!["http://localhost:8545".to_string()],
            websocket_url: "ws://localhost:8546".to_string(),
            testimony_settings: TestimonySettings {
                min_testimonies: 1,
                min_confidence: 0.5,
                timeout_secs: 10,
                ..Default::default()
            },
            ..Default::default()
        }
    }
}
