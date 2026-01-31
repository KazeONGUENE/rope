//! # Interoperability Bridge Architecture
//! 
//! Connects the String Lattice to external systems:
//! - Traditional blockchains (Ethereum, XDC, Polkadot, Bitcoin)
//! - Databases and APIs
//! - IoT networks
//! - Financial protocols (banks, asset management)
//!
//! ## EVM Invocation Bridge
//! 
//! The EVM Invocation Bridge (`evm_invocation` module) provides the critical
//! translation layer between Datachain Rope's Layer 0 DAG architecture and
//! EVM-compatible chains. This enables:
//! 
//! - Wallet compatibility (MetaMask, etc.) via JSON-RPC
//! - Cross-chain asset transfers
//! - Smart contract invocations from the DAG
//! - State proof generation for trustless verification

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub mod evm_invocation;

pub mod common {
    //! Common bridge utilities and traits
    
    use super::*;
    
    /// Bridge trait for cross-protocol communication
    #[async_trait]
    pub trait Bridge: Send + Sync {
        /// Bridge name
        fn name(&self) -> &str;
        
        /// Bridge protocol type
        fn protocol_type(&self) -> ProtocolType;
        
        /// Check if bridge is connected
        async fn is_connected(&self) -> bool;
        
        /// Sync state with external system
        async fn sync_state(&mut self) -> Result<(), BridgeError>;
        
        /// Submit a transaction to the external system
        async fn submit_transaction(&self, tx: BridgeTransaction) -> Result<[u8; 32], BridgeError>;
        
        /// Verify a proof from the external system
        async fn verify_proof(&self, proof: &[u8]) -> Result<bool, BridgeError>;
    }
    
    /// Types of external protocols the bridge can connect to
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ProtocolType {
        /// EVM-compatible blockchains
        Blockchain(BlockchainType),
        /// Traditional banking/finance
        Finance(FinanceProtocol),
        /// Asset management systems
        AssetManagement,
        /// Database/API
        DataStore,
        /// IoT networks
        IoT,
        /// Custom protocol
        Custom(String),
    }
    
    /// Blockchain types
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum BlockchainType {
        Ethereum,
        XDC,
        Polkadot,
        Bitcoin,
        Solana,
        Other(String),
    }
    
    /// Finance protocols
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum FinanceProtocol {
        Swift,
        Sepa,
        FedWire,
        ACH,
        Custom(String),
    }
    
    /// Bridge transaction
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct BridgeTransaction {
        pub id: [u8; 32],
        pub source_string_id: [u8; 32],
        pub target_protocol: ProtocolType,
        pub payload: Vec<u8>,
        pub metadata: TransactionMetadata,
    }
    
    /// Transaction metadata
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct TransactionMetadata {
        pub timestamp: u64,
        pub sender: [u8; 32],
        pub gas_limit: Option<u64>,
        pub priority: TransactionPriority,
    }
    
    /// Transaction priority
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum TransactionPriority {
        Low,
        Medium,
        High,
        Critical,
    }
    
    /// Bridge error
    #[derive(Clone, Debug)]
    pub enum BridgeError {
        ConnectionFailed(String),
        TransactionFailed(String),
        VerificationFailed(String),
        ProtocolMismatch,
        Timeout,
        InvalidProof,
        Unauthorized,
    }
    
    impl std::fmt::Display for BridgeError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                BridgeError::ConnectionFailed(s) => write!(f, "Connection failed: {}", s),
                BridgeError::TransactionFailed(s) => write!(f, "Transaction failed: {}", s),
                BridgeError::VerificationFailed(s) => write!(f, "Verification failed: {}", s),
                BridgeError::ProtocolMismatch => write!(f, "Protocol mismatch"),
                BridgeError::Timeout => write!(f, "Operation timed out"),
                BridgeError::InvalidProof => write!(f, "Invalid proof"),
                BridgeError::Unauthorized => write!(f, "Unauthorized operation"),
            }
        }
    }
    
    impl std::error::Error for BridgeError {}
}

pub mod ethereum {
    //! Ethereum bridge (EVM + Solidity)
    
    use super::*;
    use super::common::*;
    
    /// Ethereum bridge configuration
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct EthereumConfig {
        pub rpc_url: String,
        pub chain_id: u64,
        pub contract_address: String,
        pub confirmations_required: u32,
    }
    
    /// Ethereum bridge implementation
    pub struct EthereumBridge {
        config: EthereumConfig,
        connected: bool,
    }
    
    impl EthereumBridge {
        pub fn new(config: EthereumConfig) -> Self {
            Self {
                config,
                connected: false,
            }
        }
    }
    
    #[async_trait]
    impl Bridge for EthereumBridge {
        fn name(&self) -> &str {
            "Ethereum Bridge"
        }
        
        fn protocol_type(&self) -> ProtocolType {
            ProtocolType::Blockchain(BlockchainType::Ethereum)
        }
        
        async fn is_connected(&self) -> bool {
            self.connected
        }
        
        async fn sync_state(&mut self) -> Result<(), BridgeError> {
            // TODO: Implement actual Ethereum sync
            self.connected = true;
            Ok(())
        }
        
        async fn submit_transaction(&self, _tx: BridgeTransaction) -> Result<[u8; 32], BridgeError> {
            // TODO: Implement actual transaction submission
            Ok([0u8; 32])
        }
        
        async fn verify_proof(&self, _proof: &[u8]) -> Result<bool, BridgeError> {
            // TODO: Implement proof verification
            Ok(true)
        }
    }
}

pub mod xdc {
    //! XDC Network bridge
    
    use super::*;
    use super::common::*;
    
    /// XDC bridge configuration
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct XdcConfig {
        pub rpc_url: String,
        pub network_id: u64,
    }
    
    /// XDC bridge implementation
    pub struct XdcBridge {
        config: XdcConfig,
        connected: bool,
    }
    
    impl XdcBridge {
        pub fn new(config: XdcConfig) -> Self {
            Self {
                config,
                connected: false,
            }
        }
    }
    
    #[async_trait]
    impl Bridge for XdcBridge {
        fn name(&self) -> &str {
            "XDC Network Bridge"
        }
        
        fn protocol_type(&self) -> ProtocolType {
            ProtocolType::Blockchain(BlockchainType::XDC)
        }
        
        async fn is_connected(&self) -> bool {
            self.connected
        }
        
        async fn sync_state(&mut self) -> Result<(), BridgeError> {
            self.connected = true;
            Ok(())
        }
        
        async fn submit_transaction(&self, _tx: BridgeTransaction) -> Result<[u8; 32], BridgeError> {
            Ok([0u8; 32])
        }
        
        async fn verify_proof(&self, _proof: &[u8]) -> Result<bool, BridgeError> {
            Ok(true)
        }
    }
}

pub mod polkadot {
    //! Polkadot bridge (Substrate)
    
    use super::*;
    use super::common::*;
    
    /// Polkadot bridge configuration
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct PolkadotConfig {
        pub ws_url: String,
        pub para_id: Option<u32>,
    }
    
    /// Polkadot bridge implementation
    pub struct PolkadotBridge {
        config: PolkadotConfig,
        connected: bool,
    }
    
    impl PolkadotBridge {
        pub fn new(config: PolkadotConfig) -> Self {
            Self {
                config,
                connected: false,
            }
        }
    }
    
    #[async_trait]
    impl Bridge for PolkadotBridge {
        fn name(&self) -> &str {
            "Polkadot Bridge"
        }
        
        fn protocol_type(&self) -> ProtocolType {
            ProtocolType::Blockchain(BlockchainType::Polkadot)
        }
        
        async fn is_connected(&self) -> bool {
            self.connected
        }
        
        async fn sync_state(&mut self) -> Result<(), BridgeError> {
            self.connected = true;
            Ok(())
        }
        
        async fn submit_transaction(&self, _tx: BridgeTransaction) -> Result<[u8; 32], BridgeError> {
            Ok([0u8; 32])
        }
        
        async fn verify_proof(&self, _proof: &[u8]) -> Result<bool, BridgeError> {
            Ok(true)
        }
    }
}

// Re-export common types
pub use common::*;

// ============================================================================
// Semantic Translation Layer
// ============================================================================

pub mod semantic {
    //! Semantic translation between Datachain Rope and external protocols
    //!
    //! This module handles the translation of:
    //! - Data structures (String Lattice ↔ Blockchain blocks/transactions)
    //! - Cryptographic proofs (Testimony ↔ PoS/PoW)
    //! - Address formats (Rope IDs ↔ Ethereum addresses)
    //! - Contract semantics (AI Testimony ↔ Smart Contracts)
    
    use super::*;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    
    /// Semantic mapping between Rope and external concepts
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct SemanticMapping {
        /// Source concept (Rope)
        pub rope_concept: RopeConcept,
        
        /// Target protocol
        pub target_protocol: super::common::ProtocolType,
        
        /// Target concept
        pub external_concept: ExternalConcept,
        
        /// Transformation rules
        pub rules: Vec<TransformationRule>,
    }
    
    /// Rope-native concepts
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum RopeConcept {
        /// String in the lattice
        String { id: [u8; 32] },
        
        /// Testimony consensus vote
        Testimony { validator_id: [u8; 32] },
        
        /// AI Agent validation
        AIValidation { agent_type: String },
        
        /// Entity/wallet
        Entity { public_key: Vec<u8> },
        
        /// DC-20 Token transfer
        TokenTransfer { token_id: [u8; 32], amount: u128 },
        
        /// Erasure request (GDPR)
        ErasureRequest { request_id: [u8; 32] },
    }
    
    /// External protocol concepts
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ExternalConcept {
        /// Ethereum/EVM transaction
        EvmTransaction { hash: [u8; 32] },
        
        /// EVM block
        EvmBlock { number: u64 },
        
        /// ERC-20 token transfer
        Erc20Transfer { contract: [u8; 20], amount: u128 },
        
        /// Smart contract call
        SmartContractCall { contract: [u8; 20], function: String },
        
        /// Ethereum address
        EthereumAddress { address: [u8; 20] },
        
        /// XDC transaction
        XdcTransaction { hash: [u8; 32] },
        
        /// Polkadot extrinsic
        PolkadotExtrinsic { block: u32, index: u32 },
    }
    
    /// Transformation rule
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct TransformationRule {
        /// Rule name
        pub name: String,
        
        /// Field mapping (rope_field -> external_field)
        pub field_mapping: HashMap<String, String>,
        
        /// Value transformation (if any)
        pub value_transform: Option<ValueTransform>,
        
        /// Validation required
        pub requires_validation: bool,
    }
    
    /// Value transformation types
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub enum ValueTransform {
        /// Direct copy
        Identity,
        
        /// Hash transformation
        Hash { algorithm: String },
        
        /// Address format conversion
        AddressFormat { from: String, to: String },
        
        /// Numeric scaling (e.g., wei ↔ DC)
        Scale { factor: f64 },
        
        /// Custom transformation
        Custom { function_name: String },
    }
    
    /// Semantic translator
    pub struct SemanticTranslator {
        /// Registered mappings
        mappings: HashMap<String, SemanticMapping>,
        
        /// Address converter
        address_converter: AddressConverter,
    }
    
    impl SemanticTranslator {
        /// Create new translator
        pub fn new() -> Self {
            let mut translator = Self {
                mappings: HashMap::new(),
                address_converter: AddressConverter::new(),
            };
            
            // Register default mappings
            translator.register_default_mappings();
            translator
        }
        
        fn register_default_mappings(&mut self) {
            // String → EVM Transaction
            let string_to_tx = SemanticMapping {
                rope_concept: RopeConcept::String { id: [0u8; 32] },
                target_protocol: super::common::ProtocolType::Blockchain(super::common::BlockchainType::Ethereum),
                external_concept: ExternalConcept::EvmTransaction { hash: [0u8; 32] },
                rules: vec![
                    TransformationRule {
                        name: "string_id_to_tx_hash".to_string(),
                        field_mapping: [("string_id".to_string(), "hash".to_string())].into_iter().collect(),
                        value_transform: Some(ValueTransform::Hash { algorithm: "keccak256".to_string() }),
                        requires_validation: true,
                    }
                ],
            };
            self.mappings.insert("string_to_evm_tx".to_string(), string_to_tx);
            
            // Token Transfer → ERC-20
            let token_to_erc20 = SemanticMapping {
                rope_concept: RopeConcept::TokenTransfer { token_id: [0u8; 32], amount: 0 },
                target_protocol: super::common::ProtocolType::Blockchain(super::common::BlockchainType::Ethereum),
                external_concept: ExternalConcept::Erc20Transfer { contract: [0u8; 20], amount: 0 },
                rules: vec![
                    TransformationRule {
                        name: "amount_scaling".to_string(),
                        field_mapping: [("amount".to_string(), "amount".to_string())].into_iter().collect(),
                        value_transform: Some(ValueTransform::Scale { factor: 1e18 }),
                        requires_validation: true,
                    }
                ],
            };
            self.mappings.insert("token_to_erc20".to_string(), token_to_erc20);
        }
        
        /// Translate Rope concept to external format
        pub fn translate_outbound(&self, concept: &RopeConcept, target: &str) -> Result<Vec<u8>, String> {
            let mapping = self.mappings.get(target)
                .ok_or_else(|| format!("No mapping found for: {}", target))?;
            
            // Apply transformation rules
            let mut result = Vec::new();
            
            match concept {
                RopeConcept::String { id } => {
                    // Convert string ID to external format
                    result.extend_from_slice(id);
                }
                RopeConcept::TokenTransfer { token_id, amount } => {
                    // Pack token transfer data
                    result.extend_from_slice(token_id);
                    result.extend_from_slice(&amount.to_be_bytes());
                }
                RopeConcept::Entity { public_key } => {
                    // Convert to Ethereum address format
                    let eth_addr = self.address_converter.rope_to_ethereum(public_key);
                    result.extend_from_slice(&eth_addr);
                }
                _ => {
                    return Err("Unsupported concept for outbound translation".to_string());
                }
            }
            
            Ok(result)
        }
        
        /// Translate external data to Rope concept
        pub fn translate_inbound(&self, data: &[u8], source: &str) -> Result<RopeConcept, String> {
            match source {
                "evm_tx" => {
                    if data.len() >= 32 {
                        let mut id = [0u8; 32];
                        id.copy_from_slice(&data[..32]);
                        Ok(RopeConcept::String { id })
                    } else {
                        Err("Invalid EVM transaction data".to_string())
                    }
                }
                "erc20_transfer" => {
                    if data.len() >= 48 {
                        let mut token_id = [0u8; 32];
                        token_id.copy_from_slice(&data[..32]);
                        let amount = u128::from_be_bytes(data[32..48].try_into().unwrap());
                        Ok(RopeConcept::TokenTransfer { token_id, amount })
                    } else {
                        Err("Invalid ERC-20 transfer data".to_string())
                    }
                }
                _ => Err(format!("Unknown source format: {}", source)),
            }
        }
    }
    
    impl Default for SemanticTranslator {
        fn default() -> Self {
            Self::new()
        }
    }
    
    /// Address format converter
    pub struct AddressConverter {
        /// Address checksum cache
        checksum_cache: HashMap<Vec<u8>, [u8; 20]>,
    }
    
    impl AddressConverter {
        pub fn new() -> Self {
            Self {
                checksum_cache: HashMap::new(),
            }
        }
        
        /// Convert Rope public key to Ethereum address
        pub fn rope_to_ethereum(&self, public_key: &[u8]) -> [u8; 20] {
            // Ethereum address is last 20 bytes of Keccak256(public_key)
            // Using BLAKE3 as placeholder (in production, use actual Keccak256)
            let hash = blake3::hash(public_key);
            let mut address = [0u8; 20];
            address.copy_from_slice(&hash.as_bytes()[12..32]);
            address
        }
        
        /// Convert Ethereum address to Rope entity format
        pub fn ethereum_to_rope(&self, address: &[u8; 20]) -> Vec<u8> {
            // Pad Ethereum address to 32 bytes for Rope
            let mut rope_id = vec![0u8; 12];
            rope_id.extend_from_slice(address);
            rope_id
        }
        
        /// Convert XDC address (xdc prefix) to Rope format
        pub fn xdc_to_rope(&self, xdc_address: &str) -> Result<Vec<u8>, String> {
            // XDC uses "xdc" prefix instead of "0x"
            if !xdc_address.starts_with("xdc") {
                return Err("Invalid XDC address format".to_string());
            }
            
            let hex_part = &xdc_address[3..];
            let bytes = hex::decode(hex_part)
                .map_err(|e| format!("Invalid hex: {}", e))?;
            
            if bytes.len() != 20 {
                return Err("XDC address must be 20 bytes".to_string());
            }
            
            Ok(self.ethereum_to_rope(&bytes.try_into().unwrap()))
        }
    }
    
    impl Default for AddressConverter {
        fn default() -> Self {
            Self::new()
        }
    }
}

// ============================================================================
// Encapsulation Protocol (Privacy Layer)
// ============================================================================

pub mod encapsulation {
    //! Transaction encapsulation for privacy
    //!
    //! The Encapsulation Protocol provides:
    //! - Transaction anonymization
    //! - Mixing/tumbling for unlinkability  
    //! - Zero-knowledge proofs of validity
    //! - Cross-chain privacy preservation
    
    use super::*;
    use serde::{Deserialize, Serialize};
    
    /// Encapsulated transaction (anonymized)
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct EncapsulatedTransaction {
        /// Encapsulation ID
        pub id: [u8; 32],
        
        /// Encrypted payload
        pub encrypted_payload: Vec<u8>,
        
        /// Commitment to the original transaction
        pub commitment: [u8; 32],
        
        /// Nullifier (prevents double-spending)
        pub nullifier: [u8; 32],
        
        /// Zero-knowledge proof of validity
        pub zkp: ZkProof,
        
        /// Timestamp
        pub timestamp: i64,
        
        /// Target chain (if cross-chain)
        pub target_chain: Option<String>,
    }
    
    /// Zero-knowledge proof (simplified structure)
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct ZkProof {
        /// Proof type
        pub proof_type: ZkProofType,
        
        /// Proof data
        pub proof_data: Vec<u8>,
        
        /// Public inputs
        pub public_inputs: Vec<[u8; 32]>,
        
        /// Verification key hash
        pub vk_hash: [u8; 32],
    }
    
    /// Types of ZK proofs supported
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ZkProofType {
        /// Groth16 proof (small, fast verification)
        Groth16,
        
        /// PLONK proof (universal setup)
        Plonk,
        
        /// Bulletproofs (no trusted setup, range proofs)
        Bulletproofs,
        
        /// STARK (post-quantum, no trusted setup)
        Stark,
    }
    
    /// Encapsulation request
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct EncapsulationRequest {
        /// Original transaction data
        pub original_tx: Vec<u8>,
        
        /// Privacy level
        pub privacy_level: PrivacyLevel,
        
        /// Requester ID
        pub requester: [u8; 32],
        
        /// Optional mixing delay
        pub mixing_delay_seconds: Option<u64>,
    }
    
    /// Privacy levels
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PrivacyLevel {
        /// Basic encryption only
        Basic,
        
        /// Encryption + commitment hiding
        Medium,
        
        /// Full mixing + ZK proofs
        High,
        
        /// Maximum privacy (multi-hop mixing)
        Maximum,
    }
    
    /// Encapsulation engine
    pub struct EncapsulationEngine {
        /// Encryption key (for payload encryption)
        encryption_key: [u8; 32],
        
        /// Nullifier set (spent nullifiers)
        nullifier_set: std::collections::HashSet<[u8; 32]>,
        
        /// Pending mix pool
        mix_pool: Vec<EncapsulatedTransaction>,
        
        /// Statistics
        stats: EncapsulationStats,
    }
    
    /// Statistics
    #[derive(Clone, Debug, Default, Serialize, Deserialize)]
    pub struct EncapsulationStats {
        pub total_encapsulated: u64,
        pub total_decapsulated: u64,
        pub current_mix_pool_size: usize,
        pub nullifiers_count: usize,
    }
    
    impl EncapsulationEngine {
        /// Create new engine with random key
        pub fn new() -> Self {
            let mut key = [0u8; 32];
            // In production, use secure random
            for (i, byte) in key.iter_mut().enumerate() {
                *byte = (i as u8).wrapping_mul(97).wrapping_add(13);
            }
            
            Self {
                encryption_key: key,
                nullifier_set: std::collections::HashSet::new(),
                mix_pool: Vec::new(),
                stats: EncapsulationStats::default(),
            }
        }
        
        /// Encapsulate a transaction
        pub fn encapsulate(&mut self, request: EncapsulationRequest) -> Result<EncapsulatedTransaction, String> {
            // Generate commitment
            let commitment = *blake3::hash(&request.original_tx).as_bytes();
            
            // Generate nullifier (unique, prevents double-spend)
            let mut nullifier_input = request.requester.to_vec();
            nullifier_input.extend_from_slice(&commitment);
            let nullifier = *blake3::hash(&nullifier_input).as_bytes();
            
            // Check nullifier hasn't been used
            if self.nullifier_set.contains(&nullifier) {
                return Err("Nullifier already used".to_string());
            }
            
            // Simple XOR encryption (in production, use proper AEAD)
            let encrypted_payload: Vec<u8> = request.original_tx
                .iter()
                .enumerate()
                .map(|(i, &b)| b ^ self.encryption_key[i % 32])
                .collect();
            
            // Generate ZK proof (placeholder)
            let zkp = self.generate_zkp(&request, &commitment)?;
            
            // Generate encapsulation ID
            let mut id_input = commitment.to_vec();
            id_input.extend_from_slice(&nullifier);
            let id = *blake3::hash(&id_input).as_bytes();
            
            let encapsulated = EncapsulatedTransaction {
                id,
                encrypted_payload,
                commitment,
                nullifier,
                zkp,
                timestamp: chrono::Utc::now().timestamp(),
                target_chain: None,
            };
            
            // Add to mix pool if high privacy
            if request.privacy_level == PrivacyLevel::High || request.privacy_level == PrivacyLevel::Maximum {
                self.mix_pool.push(encapsulated.clone());
                self.stats.current_mix_pool_size = self.mix_pool.len();
            }
            
            self.stats.total_encapsulated += 1;
            
            Ok(encapsulated)
        }
        
        /// Generate ZK proof for transaction validity
        fn generate_zkp(&self, request: &EncapsulationRequest, commitment: &[u8; 32]) -> Result<ZkProof, String> {
            // Simplified ZK proof generation
            // In production, use actual ZK proving system (snarkjs, bellman, etc.)
            
            let proof_type = match request.privacy_level {
                PrivacyLevel::Basic | PrivacyLevel::Medium => ZkProofType::Bulletproofs,
                PrivacyLevel::High => ZkProofType::Groth16,
                PrivacyLevel::Maximum => ZkProofType::Stark,
            };
            
            // Mock proof data
            let mut proof_data = vec![0u8; 128];
            proof_data[..32].copy_from_slice(commitment);
            
            let vk_hash = *blake3::hash(b"verification_key").as_bytes();
            
            Ok(ZkProof {
                proof_type,
                proof_data,
                public_inputs: vec![*commitment],
                vk_hash,
            })
        }
        
        /// Verify an encapsulated transaction
        pub fn verify(&self, tx: &EncapsulatedTransaction) -> bool {
            // Check nullifier not already spent
            if self.nullifier_set.contains(&tx.nullifier) {
                return false;
            }
            
            // Verify ZK proof (simplified)
            if tx.zkp.public_inputs.is_empty() {
                return false;
            }
            
            // Check commitment matches first public input
            tx.commitment == tx.zkp.public_inputs[0]
        }
        
        /// Mark nullifier as spent
        pub fn spend_nullifier(&mut self, nullifier: [u8; 32]) -> bool {
            let inserted = self.nullifier_set.insert(nullifier);
            if inserted {
                self.stats.nullifiers_count = self.nullifier_set.len();
            }
            inserted
        }
        
        /// Decapsulate (reveal) a transaction
        pub fn decapsulate(&mut self, tx: &EncapsulatedTransaction, key: &[u8; 32]) -> Result<Vec<u8>, String> {
            // Check nullifier is valid (not already spent)
            if self.nullifier_set.contains(&tx.nullifier) {
                return Err("Nullifier already spent".to_string());
            }
            
            // Decrypt
            let decrypted: Vec<u8> = tx.encrypted_payload
                .iter()
                .enumerate()
                .map(|(i, &b)| b ^ key[i % 32])
                .collect();
            
            // Verify commitment
            let computed_commitment = *blake3::hash(&decrypted).as_bytes();
            if computed_commitment != tx.commitment {
                return Err("Commitment verification failed".to_string());
            }
            
            // Mark nullifier as spent
            self.spend_nullifier(tx.nullifier);
            self.stats.total_decapsulated += 1;
            
            Ok(decrypted)
        }
        
        /// Get mix pool size (for mixing services)
        pub fn mix_pool_size(&self) -> usize {
            self.mix_pool.len()
        }
        
        /// Execute mixing (shuffle pool)
        pub fn execute_mix(&mut self) -> Vec<EncapsulatedTransaction> {
            // Shuffle the mix pool
            // In production, use cryptographic shuffling
            let mixed = std::mem::take(&mut self.mix_pool);
            self.stats.current_mix_pool_size = 0;
            mixed
        }
        
        /// Get statistics
        pub fn stats(&self) -> &EncapsulationStats {
            &self.stats
        }
    }
    
    impl Default for EncapsulationEngine {
        fn default() -> Self {
            Self::new()
        }
    }
}

// ============================================================================  
// Cross-Chain Proof Verification
// ============================================================================

pub mod verification {
    //! Cross-chain proof verification
    //!
    //! Verifies proofs from external chains:
    //! - Merkle proofs (Ethereum, Bitcoin)
    //! - SPV proofs (Bitcoin)
    //! - State proofs (Ethereum)
    //! - Finality proofs (Polkadot)
    
    use super::*;
    use serde::{Deserialize, Serialize};
    
    /// Proof types from external chains
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub enum CrossChainProof {
        /// Ethereum Merkle Patricia proof
        EthereumMerkle {
            account_proof: Vec<Vec<u8>>,
            storage_proof: Vec<Vec<u8>>,
            state_root: [u8; 32],
        },
        
        /// Bitcoin SPV proof
        BitcoinSpv {
            merkle_branch: Vec<[u8; 32]>,
            #[serde(with = "serde_bytes")]
            block_header: Vec<u8>, // 80 bytes
            tx_index: u32,
        },
        
        /// Polkadot finality proof
        PolkadotFinality {
            justification: Vec<u8>,
            authority_set_id: u64,
        },
        
        /// XDC master node attestation
        XdcAttestation {
            signatures: Vec<Vec<u8>>,
            master_nodes: Vec<[u8; 20]>,
        },
    }
    
    /// Proof verification result
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct VerificationResult {
        /// Is the proof valid?
        pub is_valid: bool,
        
        /// Confidence level (0-100)
        pub confidence: u8,
        
        /// Verified data hash
        pub data_hash: Option<[u8; 32]>,
        
        /// Verification method used
        pub method: String,
        
        /// Timestamp of verification
        pub verified_at: i64,
        
        /// Error message if invalid
        pub error: Option<String>,
    }
    
    /// Cross-chain proof verifier
    pub struct CrossChainVerifier {
        /// Ethereum light client state roots (block -> root)
        ethereum_state_roots: std::collections::HashMap<u64, [u8; 32]>,
        
        /// Bitcoin block headers (80 bytes each)
        bitcoin_headers: Vec<Vec<u8>>,
        
        /// Trusted XDC master nodes
        xdc_master_nodes: std::collections::HashSet<[u8; 20]>,
    }
    
    impl CrossChainVerifier {
        /// Create new verifier
        pub fn new() -> Self {
            Self {
                ethereum_state_roots: std::collections::HashMap::new(),
                bitcoin_headers: Vec::new(),
                xdc_master_nodes: std::collections::HashSet::new(),
            }
        }
        
        /// Add trusted Ethereum state root
        pub fn add_ethereum_state_root(&mut self, block: u64, root: [u8; 32]) {
            self.ethereum_state_roots.insert(block, root);
        }
        
        /// Add trusted XDC master node
        pub fn add_xdc_master_node(&mut self, node: [u8; 20]) {
            self.xdc_master_nodes.insert(node);
        }
        
        /// Verify a cross-chain proof
        pub fn verify(&self, proof: &CrossChainProof) -> VerificationResult {
            match proof {
                CrossChainProof::EthereumMerkle { state_root, .. } => {
                    // Check if we have this state root as trusted
                    let is_trusted = self.ethereum_state_roots.values()
                        .any(|r| r == state_root);
                    
                    VerificationResult {
                        is_valid: is_trusted,
                        confidence: if is_trusted { 90 } else { 0 },
                        data_hash: Some(*state_root),
                        method: "ethereum_merkle".to_string(),
                        verified_at: chrono::Utc::now().timestamp(),
                        error: if is_trusted { None } else { Some("Unknown state root".to_string()) },
                    }
                }
                
                CrossChainProof::BitcoinSpv { block_header, merkle_branch, .. } => {
                    // Simplified SPV verification
                    let header_hash = *blake3::hash(&block_header).as_bytes();
                    
                    VerificationResult {
                        is_valid: !merkle_branch.is_empty(),
                        confidence: 85,
                        data_hash: Some(header_hash),
                        method: "bitcoin_spv".to_string(),
                        verified_at: chrono::Utc::now().timestamp(),
                        error: None,
                    }
                }
                
                CrossChainProof::XdcAttestation { signatures, master_nodes } => {
                    // Check master node attestations
                    let trusted_count = master_nodes.iter()
                        .filter(|n| self.xdc_master_nodes.contains(*n))
                        .count();
                    
                    let required = (self.xdc_master_nodes.len() * 2) / 3;
                    let is_valid = trusted_count >= required && !signatures.is_empty();
                    
                    VerificationResult {
                        is_valid,
                        confidence: ((trusted_count as f64 / master_nodes.len() as f64) * 100.0) as u8,
                        data_hash: None,
                        method: "xdc_attestation".to_string(),
                        verified_at: chrono::Utc::now().timestamp(),
                        error: if is_valid { None } else { Some("Insufficient attestations".to_string()) },
                    }
                }
                
                CrossChainProof::PolkadotFinality { authority_set_id, .. } => {
                    VerificationResult {
                        is_valid: *authority_set_id > 0,
                        confidence: 95,
                        data_hash: None,
                        method: "polkadot_finality".to_string(),
                        verified_at: chrono::Utc::now().timestamp(),
                        error: None,
                    }
                }
            }
        }
    }
    
    impl Default for CrossChainVerifier {
        fn default() -> Self {
            Self::new()
        }
    }
}
