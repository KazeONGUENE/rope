//! # Protocol Adapters
//! 
//! Adapters for connecting to various external protocols:
//! - Blockchain networks
//! - Banking systems
//! - Finance protocols
//! - Asset management platforms

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Trait for protocol adapters
#[async_trait]
pub trait ProtocolAdapter: Send + Sync {
    /// Protocol name
    fn name(&self) -> &str;
    
    /// Protocol type
    fn protocol_type(&self) -> ProtocolType;
    
    /// Connect to protocol
    async fn connect(&mut self) -> Result<(), AdapterError>;
    
    /// Disconnect
    async fn disconnect(&mut self) -> Result<(), AdapterError>;
    
    /// Check connection status
    fn is_connected(&self) -> bool;
    
    /// Submit transaction
    async fn submit_transaction(&self, tx: &ProtocolTransaction) -> Result<TransactionReceipt, AdapterError>;
    
    /// Query state
    async fn query(&self, query: &ProtocolQuery) -> Result<QueryResult, AdapterError>;
}

/// Protocol types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProtocolType {
    Blockchain(BlockchainType),
    Banking(BankingType),
    Finance(FinanceType),
    AssetManagement,
    Identity,
    Custom(String),
}

/// Blockchain types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BlockchainType {
    Ethereum { chain_id: u64 },
    Bitcoin { network: String },
    Polkadot { para_id: Option<u32> },
    XDC,
    Solana,
    Other(String),
}

/// Banking protocol types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BankingType {
    Swift,
    Sepa,
    Ach,
    FedWire,
    OpenBanking { api_version: String },
}

/// Finance protocol types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinanceType {
    Fix { version: String },
    Bloomberg,
    Refinitiv,
    Trading,
}

/// Transaction for a protocol
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolTransaction {
    pub id: [u8; 32],
    pub from: String,
    pub to: String,
    pub operation: TransactionOperation,
    pub parameters: HashMap<String, TransactionValue>,
    pub metadata: HashMap<String, String>,
}

/// Transaction operations
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransactionOperation {
    Transfer { asset: String, amount: String },
    ContractCall { method: String, args: Vec<TransactionValue> },
    Query { query_type: String },
    Sign { message: Vec<u8> },
    Custom(String),
}

/// Transaction parameter value
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TransactionValue {
    String(String),
    Integer(i64),
    Float(f64),
    Bytes(Vec<u8>),
    Boolean(bool),
    Array(Vec<TransactionValue>),
    Map(HashMap<String, TransactionValue>),
}

/// Transaction receipt
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionReceipt {
    pub tx_hash: [u8; 32],
    pub status: TransactionStatus,
    pub block_number: Option<u64>,
    pub gas_used: Option<u64>,
    pub logs: Vec<TransactionLog>,
    pub timestamp: i64,
}

/// Transaction status
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransactionStatus {
    Pending,
    Confirmed,
    Failed { reason: String },
}

/// Transaction log entry
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionLog {
    pub index: u32,
    pub topics: Vec<[u8; 32]>,
    pub data: Vec<u8>,
}

/// Protocol query
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProtocolQuery {
    pub query_type: QueryType,
    pub parameters: HashMap<String, TransactionValue>,
}

/// Query types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum QueryType {
    Balance { address: String },
    Transaction { tx_hash: [u8; 32] },
    Block { number: u64 },
    ContractState { contract: String, method: String },
    Custom(String),
}

/// Query result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QueryResult {
    pub success: bool,
    pub data: TransactionValue,
    pub error: Option<String>,
}

/// Adapter errors
#[derive(Clone, Debug)]
pub enum AdapterError {
    ConnectionFailed(String),
    NotConnected,
    TransactionFailed(String),
    QueryFailed(String),
    InvalidParameter(String),
    Timeout,
    RateLimited,
}

impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AdapterError::ConnectionFailed(s) => write!(f, "Connection failed: {}", s),
            AdapterError::NotConnected => write!(f, "Not connected"),
            AdapterError::TransactionFailed(s) => write!(f, "Transaction failed: {}", s),
            AdapterError::QueryFailed(s) => write!(f, "Query failed: {}", s),
            AdapterError::InvalidParameter(s) => write!(f, "Invalid parameter: {}", s),
            AdapterError::Timeout => write!(f, "Operation timed out"),
            AdapterError::RateLimited => write!(f, "Rate limited"),
        }
    }
}

impl std::error::Error for AdapterError {}

// === Concrete Adapter Implementations ===

/// Ethereum adapter
pub struct EthereumAdapter {
    rpc_url: String,
    chain_id: u64,
    connected: bool,
}

impl EthereumAdapter {
    pub fn new(rpc_url: String, chain_id: u64) -> Self {
        Self {
            rpc_url,
            chain_id,
            connected: false,
        }
    }
}

#[async_trait]
impl ProtocolAdapter for EthereumAdapter {
    fn name(&self) -> &str {
        "Ethereum"
    }
    
    fn protocol_type(&self) -> ProtocolType {
        ProtocolType::Blockchain(BlockchainType::Ethereum { chain_id: self.chain_id })
    }
    
    async fn connect(&mut self) -> Result<(), AdapterError> {
        // In production: Connect to RPC endpoint
        self.connected = true;
        Ok(())
    }
    
    async fn disconnect(&mut self) -> Result<(), AdapterError> {
        self.connected = false;
        Ok(())
    }
    
    fn is_connected(&self) -> bool {
        self.connected
    }
    
    async fn submit_transaction(&self, tx: &ProtocolTransaction) -> Result<TransactionReceipt, AdapterError> {
        if !self.connected {
            return Err(AdapterError::NotConnected);
        }
        
        // In production: Use ethers-rs to submit transaction
        Ok(TransactionReceipt {
            tx_hash: tx.id,
            status: TransactionStatus::Confirmed,
            block_number: Some(12345678),
            gas_used: Some(21000),
            logs: Vec::new(),
            timestamp: chrono::Utc::now().timestamp(),
        })
    }
    
    async fn query(&self, query: &ProtocolQuery) -> Result<QueryResult, AdapterError> {
        if !self.connected {
            return Err(AdapterError::NotConnected);
        }
        
        Ok(QueryResult {
            success: true,
            data: TransactionValue::String("0".to_string()),
            error: None,
        })
    }
}

/// SWIFT banking adapter
pub struct SwiftAdapter {
    endpoint: String,
    credentials: SwiftCredentials,
    connected: bool,
}

/// SWIFT credentials
#[derive(Clone, Debug)]
pub struct SwiftCredentials {
    pub bic: String,
    pub cert_path: String,
}

impl SwiftAdapter {
    pub fn new(endpoint: String, credentials: SwiftCredentials) -> Self {
        Self {
            endpoint,
            credentials,
            connected: false,
        }
    }
}

#[async_trait]
impl ProtocolAdapter for SwiftAdapter {
    fn name(&self) -> &str {
        "SWIFT"
    }
    
    fn protocol_type(&self) -> ProtocolType {
        ProtocolType::Banking(BankingType::Swift)
    }
    
    async fn connect(&mut self) -> Result<(), AdapterError> {
        // In production: Connect to SWIFT network
        self.connected = true;
        Ok(())
    }
    
    async fn disconnect(&mut self) -> Result<(), AdapterError> {
        self.connected = false;
        Ok(())
    }
    
    fn is_connected(&self) -> bool {
        self.connected
    }
    
    async fn submit_transaction(&self, tx: &ProtocolTransaction) -> Result<TransactionReceipt, AdapterError> {
        if !self.connected {
            return Err(AdapterError::NotConnected);
        }
        
        // In production: Send MT103 message
        Ok(TransactionReceipt {
            tx_hash: tx.id,
            status: TransactionStatus::Pending,
            block_number: None,
            gas_used: None,
            logs: Vec::new(),
            timestamp: chrono::Utc::now().timestamp(),
        })
    }
    
    async fn query(&self, _query: &ProtocolQuery) -> Result<QueryResult, AdapterError> {
        if !self.connected {
            return Err(AdapterError::NotConnected);
        }
        
        Ok(QueryResult {
            success: true,
            data: TransactionValue::String("OK".to_string()),
            error: None,
        })
    }
}

/// Asset management adapter
pub struct AssetManagementAdapter {
    api_url: String,
    api_key: String,
    connected: bool,
}

impl AssetManagementAdapter {
    pub fn new(api_url: String, api_key: String) -> Self {
        Self {
            api_url,
            api_key,
            connected: false,
        }
    }
}

#[async_trait]
impl ProtocolAdapter for AssetManagementAdapter {
    fn name(&self) -> &str {
        "Asset Management"
    }
    
    fn protocol_type(&self) -> ProtocolType {
        ProtocolType::AssetManagement
    }
    
    async fn connect(&mut self) -> Result<(), AdapterError> {
        self.connected = true;
        Ok(())
    }
    
    async fn disconnect(&mut self) -> Result<(), AdapterError> {
        self.connected = false;
        Ok(())
    }
    
    fn is_connected(&self) -> bool {
        self.connected
    }
    
    async fn submit_transaction(&self, tx: &ProtocolTransaction) -> Result<TransactionReceipt, AdapterError> {
        if !self.connected {
            return Err(AdapterError::NotConnected);
        }
        
        Ok(TransactionReceipt {
            tx_hash: tx.id,
            status: TransactionStatus::Confirmed,
            block_number: None,
            gas_used: None,
            logs: Vec::new(),
            timestamp: chrono::Utc::now().timestamp(),
        })
    }
    
    async fn query(&self, _query: &ProtocolQuery) -> Result<QueryResult, AdapterError> {
        if !self.connected {
            return Err(AdapterError::NotConnected);
        }
        
        Ok(QueryResult {
            success: true,
            data: TransactionValue::Map(HashMap::new()),
            error: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_ethereum_adapter() {
        let mut adapter = EthereumAdapter::new(
            "https://eth.example.com".to_string(),
            1,
        );
        
        assert!(!adapter.is_connected());
        
        adapter.connect().await.unwrap();
        assert!(adapter.is_connected());
        
        adapter.disconnect().await.unwrap();
        assert!(!adapter.is_connected());
    }
}

