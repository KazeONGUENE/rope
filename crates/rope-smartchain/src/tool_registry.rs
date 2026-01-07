//! # Vetted Tool Registry
//! 
//! The Smartchain can invoke external tools to execute transactions.
//! All tools must be registered, audited, and continuously monitored.
//! 
//! ## Tool Categories
//! 
//! - **Blockchain Tools**: Ethereum, Polkadot, XDC, Bitcoin, etc.
//! - **Banking Tools**: SWIFT, SEPA, ACH, FedWire
//! - **Finance Tools**: Trading platforms, asset management
//! - **Custom Tools**: Any vetted external service
//! 
//! ## Vetting Process
//! 
//! 1. Tool submitted by developer with audit report
//! 2. Federation governance votes on acceptance
//! 3. Security review by multiple AI agents
//! 4. Staged rollout (testnet → mainnet)
//! 5. Continuous monitoring and scoring

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use parking_lot::RwLock;

/// Vetted tool interface
#[async_trait]
pub trait VettedTool: Send + Sync {
    /// Tool identifier
    fn tool_id(&self) -> &ToolId;
    
    /// Tool metadata
    fn metadata(&self) -> &ToolMetadata;
    
    /// Check if tool can handle an action
    fn can_handle(&self, action: &ToolAction) -> bool;
    
    /// Execute an action
    async fn execute(&self, action: &ToolAction, context: &ExecutionContext) -> ExecutionResult;
    
    /// Health check
    async fn health_check(&self) -> ToolHealth;
    
    /// Get current rate limits
    fn rate_limits(&self) -> &RateLimits;
}

/// Tool identifier
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ToolId {
    /// Unique identifier
    pub id: [u8; 32],
    /// Human-readable name
    pub name: String,
    /// Version
    pub version: String,
}

impl ToolId {
    pub fn new(name: &str, version: &str) -> Self {
        let mut input = name.as_bytes().to_vec();
        input.extend_from_slice(version.as_bytes());
        Self {
            id: *blake3::hash(&input).as_bytes(),
            name: name.to_string(),
            version: version.to_string(),
        }
    }
}

/// Tool metadata
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolMetadata {
    /// Tool category
    pub category: ToolCategory,
    
    /// Description
    pub description: String,
    
    /// Developer/maintainer
    pub developer: String,
    
    /// Supported protocols
    pub protocols: Vec<SupportedProtocol>,
    
    /// Required permissions
    pub permissions: Vec<ToolPermission>,
    
    /// Audit information
    pub audit: AuditInfo,
    
    /// Trust score (0-100)
    pub trust_score: u8,
    
    /// Is tool currently active?
    pub is_active: bool,
}

/// Tool categories
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolCategory {
    /// Blockchain interactions
    Blockchain(BlockchainProtocol),
    /// Traditional banking
    Banking(BankingProtocol),
    /// Financial services
    Finance(FinanceProtocol),
    /// Asset management
    AssetManagement,
    /// Identity services
    Identity,
    /// Oracle/data feed
    Oracle,
    /// Custom category
    Custom(String),
}

/// Blockchain protocols
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockchainProtocol {
    Ethereum,
    Bitcoin,
    Polkadot,
    XDC,
    Solana,
    Avalanche,
    Polygon,
    BNBChain,
    Other(String),
}

/// Banking protocols
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BankingProtocol {
    Swift,
    Sepa,
    Ach,
    FedWire,
    Rtgs,
    OpenBanking,
    Other(String),
}

/// Finance protocols
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinanceProtocol {
    Fix,
    Bloomberg,
    Refinitiv,
    Custody,
    Trading,
    Other(String),
}

/// Supported protocol
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SupportedProtocol {
    pub protocol_type: ToolCategory,
    pub version: String,
    pub endpoints: Vec<String>,
}

/// Tool permissions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolPermission {
    /// Can read data
    Read,
    /// Can submit transactions
    Write,
    /// Can manage keys
    KeyManagement,
    /// Can access external networks
    NetworkAccess,
    /// Can store data
    Storage,
    /// Custom permission
    Custom(String),
}

/// Audit information
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditInfo {
    /// Auditor name
    pub auditor: String,
    /// Audit date
    pub audit_date: i64,
    /// Audit report hash
    pub report_hash: [u8; 32],
    /// Audit score (0-100)
    pub score: u8,
    /// Next audit due date
    pub next_audit_due: i64,
}

/// Action to execute via tool
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolAction {
    /// Action ID
    pub id: [u8; 32],
    
    /// Action type
    pub action_type: ToolActionType,
    
    /// Source entity
    pub from: [u8; 32],
    
    /// Target entity/address
    pub to: String,
    
    /// Parameters
    pub parameters: HashMap<String, ActionValue>,
    
    /// Contract reference (if from smart contract)
    pub contract_ref: Option<[u8; 32]>,
    
    /// Priority
    pub priority: ActionPriority,
    
    /// Timeout in seconds
    pub timeout_secs: u64,
}

/// Action types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolActionType {
    /// Transfer assets
    Transfer { asset: String, amount: String },
    /// Call smart contract
    ContractCall { method: String },
    /// Query state
    Query { query_type: String },
    /// Sign transaction
    Sign { tx_hash: [u8; 32] },
    /// Mint/burn tokens
    TokenOperation { op: TokenOp },
    /// Custom action
    Custom(String),
}

/// Token operations
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenOp {
    Mint,
    Burn,
    Freeze,
    Unfreeze,
}

/// Action parameter value
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ActionValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Bytes(Vec<u8>),
}

/// Action priority
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionPriority {
    Low,
    Normal,
    High,
    Critical,
}

/// Execution context
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionContext {
    /// Caller node ID
    pub caller: [u8; 32],
    /// Timestamp
    pub timestamp: i64,
    /// Gas/fee budget
    pub fee_budget: Option<u64>,
    /// Signatures from testimonies
    pub testimony_signatures: Vec<Vec<u8>>,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Execution result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Was execution successful?
    pub success: bool,
    
    /// Transaction hash (if applicable)
    pub tx_hash: Option<[u8; 32]>,
    
    /// Result data
    pub data: Option<Vec<u8>>,
    
    /// Error message (if failed)
    pub error: Option<String>,
    
    /// Gas/fee used
    pub fee_used: Option<u64>,
    
    /// Execution time in ms
    pub execution_time_ms: u64,
    
    /// Proof of execution
    pub proof: Option<ExecutionProof>,
}

/// Proof of execution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExecutionProof {
    pub proof_type: ProofType,
    pub data: Vec<u8>,
    pub verifier: String,
}

/// Proof types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProofType {
    TransactionReceipt,
    MerkleProof,
    ZkProof,
    SignatureAttestation,
}

/// Tool health status
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolHealth {
    pub is_healthy: bool,
    pub latency_ms: u64,
    pub error_rate: f64,
    pub last_success: Option<i64>,
    pub last_error: Option<String>,
}

/// Rate limits for a tool
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RateLimits {
    pub requests_per_second: u32,
    pub requests_per_minute: u32,
    pub requests_per_day: u32,
    pub concurrent_requests: u32,
}

impl Default for RateLimits {
    fn default() -> Self {
        Self {
            requests_per_second: 10,
            requests_per_minute: 100,
            requests_per_day: 10000,
            concurrent_requests: 5,
        }
    }
}

/// Tool Registry - Manages all vetted tools
pub struct ToolRegistry {
    tools: RwLock<HashMap<[u8; 32], Arc<dyn VettedTool>>>,
    metadata: RwLock<HashMap<[u8; 32], ToolMetadata>>,
    health_cache: RwLock<HashMap<[u8; 32], ToolHealth>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: RwLock::new(HashMap::new()),
            metadata: RwLock::new(HashMap::new()),
            health_cache: RwLock::new(HashMap::new()),
        }
    }
    
    /// Register a new vetted tool
    pub fn register_tool(&self, tool: Arc<dyn VettedTool>) -> Result<(), RegistryError> {
        let id = tool.tool_id().id;
        let metadata = tool.metadata().clone();
        
        // Verify minimum trust score
        if metadata.trust_score < 50 {
            return Err(RegistryError::InsufficientTrustScore(metadata.trust_score));
        }
        
        // Check audit is current
        let now = chrono::Utc::now().timestamp();
        if now > metadata.audit.next_audit_due {
            return Err(RegistryError::AuditExpired);
        }
        
        self.tools.write().insert(id, tool);
        self.metadata.write().insert(id, metadata);
        
        Ok(())
    }
    
    /// Get a tool by ID
    pub fn get_tool(&self, id: &[u8; 32]) -> Option<Arc<dyn VettedTool>> {
        self.tools.read().get(id).cloned()
    }
    
    /// Find tools by category
    pub fn find_by_category(&self, category: &ToolCategory) -> Vec<Arc<dyn VettedTool>> {
        let tools = self.tools.read();
        let metadata = self.metadata.read();
        
        let mut result = Vec::new();
        for (id, tool) in tools.iter() {
            if let Some(meta) = metadata.get(id) {
                if &meta.category == category {
                    result.push(tool.clone());
                }
            }
        }
        result
    }
    
    /// Find best tool for an action
    pub fn find_best_tool_for_action(&self, action: &ToolAction) -> Option<Arc<dyn VettedTool>> {
        let tools = self.tools.read();
        let metadata = self.metadata.read();
        let health = self.health_cache.read();
        
        let mut best_tool: Option<Arc<dyn VettedTool>> = None;
        let mut best_score = 0u8;
        
        for (id, tool) in tools.iter() {
            if tool.can_handle(action) {
                let trust = metadata.get(id).map(|m| m.trust_score).unwrap_or(0);
                let healthy = health.get(id).map(|h| if h.is_healthy { 50u8 } else { 0u8 }).unwrap_or(25);
                let score = trust + healthy;
                
                if score > best_score {
                    best_score = score;
                    best_tool = Some(tool.clone());
                }
            }
        }
        
        best_tool
    }
    
    /// Update health cache for a tool
    pub async fn update_health(&self, id: &[u8; 32]) -> Option<ToolHealth> {
        let tool = self.get_tool(id)?;
        let health = tool.health_check().await;
        self.health_cache.write().insert(*id, health.clone());
        Some(health)
    }
    
    /// Get all active tools
    pub fn list_active_tools(&self) -> Vec<ToolMetadata> {
        self.metadata.read()
            .values()
            .filter(|m| m.is_active)
            .cloned()
            .collect()
    }
    
    /// Deactivate a tool
    pub fn deactivate_tool(&self, id: &[u8; 32]) -> bool {
        if let Some(meta) = self.metadata.write().get_mut(id) {
            meta.is_active = false;
            true
        } else {
            false
        }
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Registry errors
#[derive(Clone, Debug)]
pub enum RegistryError {
    InsufficientTrustScore(u8),
    AuditExpired,
    AlreadyRegistered,
    NotFound,
    PermissionDenied,
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RegistryError::InsufficientTrustScore(s) => {
                write!(f, "Insufficient trust score: {} (minimum 50)", s)
            }
            RegistryError::AuditExpired => write!(f, "Tool audit has expired"),
            RegistryError::AlreadyRegistered => write!(f, "Tool already registered"),
            RegistryError::NotFound => write!(f, "Tool not found"),
            RegistryError::PermissionDenied => write!(f, "Permission denied"),
        }
    }
}

impl std::error::Error for RegistryError {}

// === Example Tool Implementations ===

/// Ethereum tool implementation
pub struct EthereumTool {
    id: ToolId,
    metadata: ToolMetadata,
    rate_limits: RateLimits,
    rpc_url: String,
}

impl EthereumTool {
    pub fn new(rpc_url: String, audit: AuditInfo) -> Self {
        Self {
            id: ToolId::new("ethereum-bridge", "1.0.0"),
            metadata: ToolMetadata {
                category: ToolCategory::Blockchain(BlockchainProtocol::Ethereum),
                description: "Ethereum mainnet bridge for transactions".to_string(),
                developer: "Datachain Rope Core".to_string(),
                protocols: vec![SupportedProtocol {
                    protocol_type: ToolCategory::Blockchain(BlockchainProtocol::Ethereum),
                    version: "EIP-1559".to_string(),
                    endpoints: vec![rpc_url.clone()],
                }],
                permissions: vec![ToolPermission::Read, ToolPermission::Write],
                audit,
                trust_score: 95,
                is_active: true,
            },
            rate_limits: RateLimits {
                requests_per_second: 50,
                requests_per_minute: 500,
                requests_per_day: 100000,
                concurrent_requests: 20,
            },
            rpc_url,
        }
    }
}

#[async_trait]
impl VettedTool for EthereumTool {
    fn tool_id(&self) -> &ToolId {
        &self.id
    }
    
    fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }
    
    fn can_handle(&self, action: &ToolAction) -> bool {
        matches!(action.action_type, 
            ToolActionType::Transfer { .. } | 
            ToolActionType::ContractCall { .. } |
            ToolActionType::Query { .. }
        )
    }
    
    async fn execute(&self, action: &ToolAction, _context: &ExecutionContext) -> ExecutionResult {
        // In production: Use ethers-rs to submit transaction
        let start = std::time::Instant::now();
        
        ExecutionResult {
            success: true,
            tx_hash: Some(*blake3::hash(&action.id).as_bytes()),
            data: None,
            error: None,
            fee_used: Some(21000),
            execution_time_ms: start.elapsed().as_millis() as u64,
            proof: Some(ExecutionProof {
                proof_type: ProofType::TransactionReceipt,
                data: Vec::new(),
                verifier: "ethereum-mainnet".to_string(),
            }),
        }
    }
    
    async fn health_check(&self) -> ToolHealth {
        ToolHealth {
            is_healthy: true,
            latency_ms: 50,
            error_rate: 0.001,
            last_success: Some(chrono::Utc::now().timestamp()),
            last_error: None,
        }
    }
    
    fn rate_limits(&self) -> &RateLimits {
        &self.rate_limits
    }
}

/// Banking tool (SWIFT) implementation
pub struct SwiftTool {
    id: ToolId,
    metadata: ToolMetadata,
    rate_limits: RateLimits,
}

impl SwiftTool {
    pub fn new(audit: AuditInfo) -> Self {
        Self {
            id: ToolId::new("swift-gateway", "1.0.0"),
            metadata: ToolMetadata {
                category: ToolCategory::Banking(BankingProtocol::Swift),
                description: "SWIFT gateway for international transfers".to_string(),
                developer: "Datachain Rope Finance".to_string(),
                protocols: vec![SupportedProtocol {
                    protocol_type: ToolCategory::Banking(BankingProtocol::Swift),
                    version: "MT103".to_string(),
                    endpoints: Vec::new(),
                }],
                permissions: vec![ToolPermission::Read, ToolPermission::Write],
                audit,
                trust_score: 90,
                is_active: true,
            },
            rate_limits: RateLimits {
                requests_per_second: 5,
                requests_per_minute: 50,
                requests_per_day: 1000,
                concurrent_requests: 2,
            },
        }
    }
}

#[async_trait]
impl VettedTool for SwiftTool {
    fn tool_id(&self) -> &ToolId {
        &self.id
    }
    
    fn metadata(&self) -> &ToolMetadata {
        &self.metadata
    }
    
    fn can_handle(&self, action: &ToolAction) -> bool {
        matches!(action.action_type, ToolActionType::Transfer { .. })
    }
    
    async fn execute(&self, action: &ToolAction, _context: &ExecutionContext) -> ExecutionResult {
        let start = std::time::Instant::now();
        
        // In production: Connect to SWIFT network
        ExecutionResult {
            success: true,
            tx_hash: Some(*blake3::hash(&action.id).as_bytes()),
            data: Some(b"SWIFT_REF_123456".to_vec()),
            error: None,
            fee_used: None,
            execution_time_ms: start.elapsed().as_millis() as u64,
            proof: Some(ExecutionProof {
                proof_type: ProofType::SignatureAttestation,
                data: Vec::new(),
                verifier: "swift-network".to_string(),
            }),
        }
    }
    
    async fn health_check(&self) -> ToolHealth {
        ToolHealth {
            is_healthy: true,
            latency_ms: 200,
            error_rate: 0.0001,
            last_success: Some(chrono::Utc::now().timestamp()),
            last_error: None,
        }
    }
    
    fn rate_limits(&self) -> &RateLimits {
        &self.rate_limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn test_audit() -> AuditInfo {
        AuditInfo {
            auditor: "Test Auditor".to_string(),
            audit_date: chrono::Utc::now().timestamp(),
            report_hash: [0u8; 32],
            score: 95,
            next_audit_due: chrono::Utc::now().timestamp() + 365 * 24 * 3600,
        }
    }
    
    #[test]
    fn test_tool_registry() {
        let registry = ToolRegistry::new();
        let tool = Arc::new(EthereumTool::new(
            "https://eth.example.com".to_string(),
            test_audit(),
        ));
        
        assert!(registry.register_tool(tool).is_ok());
    }
    
    #[tokio::test]
    async fn test_ethereum_tool() {
        let tool = EthereumTool::new(
            "https://eth.example.com".to_string(),
            test_audit(),
        );
        
        let action = ToolAction {
            id: [0u8; 32],
            action_type: ToolActionType::Transfer {
                asset: "ETH".to_string(),
                amount: "1.0".to_string(),
            },
            from: [0u8; 32],
            to: "0x1234...".to_string(),
            parameters: HashMap::new(),
            contract_ref: None,
            priority: ActionPriority::Normal,
            timeout_secs: 60,
        };
        
        assert!(tool.can_handle(&action));
        
        let health = tool.health_check().await;
        assert!(health.is_healthy);
    }
}

