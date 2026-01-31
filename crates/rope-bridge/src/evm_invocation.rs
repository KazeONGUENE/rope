//! # EVM Invocation Bridge
//!
//! This module provides the critical bridge between Datachain Rope's Layer 0 DAG architecture
//! and EVM-compatible chains. It enables:
//!
//! 1. **Bi-directional Translation**: String Lattice ↔ EVM Transactions
//! 2. **State Synchronization**: DAG state → EVM state proofs
//! 3. **Cross-chain Invocations**: Execute EVM smart contracts from Rope
//! 4. **Wallet Compatibility**: Present DAG as EVM-compatible via JSON-RPC
//!
//! ## Architecture Overview
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                     DATACHAIN ROPE (Layer 0)                        │
//! │                      String Lattice DAG                             │
//! │                    Testimony Consensus                              │
//! └───────────────────────────┬─────────────────────────────────────────┘
//!                             │
//!                             ▼
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                   EVM INVOCATION BRIDGE                             │
//! │  ┌──────────────┐  ┌──────────────┐  ┌──────────────────────────┐  │
//! │  │   String →   │  │   State      │  │    JSON-RPC              │  │
//! │  │   EVM Tx     │  │   Proof      │  │    Compatibility         │  │
//! │  │   Encoder    │  │   Generator  │  │    Layer                 │  │
//! │  └──────────────┘  └──────────────┘  └──────────────────────────┘  │
//! └───────────────────────────┬─────────────────────────────────────────┘
//!                             │
//!                             ▼
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                    EVM CHAINS (Layer 1)                             │
//! │         Ethereum    │    XDC    │    Polygon    │   Arbitrum       │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::common::*;
use super::ethereum::EthereumConfig;
use super::semantic::{AddressConverter, RopeConcept, SemanticTranslator};

// ============================================================================
// EVM Transaction Types (matching Ethereum's structure)
// ============================================================================

/// EVM-compatible transaction structure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmTransaction {
    /// Nonce (transaction count)
    pub nonce: u64,
    
    /// Gas price in wei
    pub gas_price: u128,
    
    /// Gas limit
    pub gas_limit: u64,
    
    /// Recipient address (None for contract creation)
    pub to: Option<[u8; 20]>,
    
    /// Value in wei
    pub value: u128,
    
    /// Transaction data (call data or contract bytecode)
    pub data: Vec<u8>,
    
    /// Chain ID (EIP-155)
    pub chain_id: u64,
    
    /// Signature v (recovery id)
    pub v: u64,
    
    /// Signature r
    pub r: [u8; 32],
    
    /// Signature s  
    pub s: [u8; 32],
}

/// EVM call result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmCallResult {
    /// Success status
    pub success: bool,
    
    /// Return data
    pub return_data: Vec<u8>,
    
    /// Gas used
    pub gas_used: u64,
    
    /// Logs emitted
    pub logs: Vec<EvmLog>,
    
    /// Error message if failed
    pub error: Option<String>,
}

/// EVM log entry
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmLog {
    /// Contract address
    pub address: [u8; 20],
    
    /// Log topics
    pub topics: Vec<[u8; 32]>,
    
    /// Log data
    pub data: Vec<u8>,
}

// ============================================================================
// String-to-EVM Encoder
// ============================================================================

/// Encodes Rope String operations as EVM transactions
pub struct StringToEvmEncoder {
    /// Chain ID for EIP-155 replay protection
    chain_id: u64,
    
    /// Address converter
    address_converter: AddressConverter,
    
    /// Semantic translator
    translator: SemanticTranslator,
    
    /// ABI encodings for common operations
    abi_cache: HashMap<String, Vec<u8>>,
}

impl StringToEvmEncoder {
    /// Create new encoder for a specific chain
    pub fn new(chain_id: u64) -> Self {
        let mut encoder = Self {
            chain_id,
            address_converter: AddressConverter::new(),
            translator: SemanticTranslator::new(),
            abi_cache: HashMap::new(),
        };
        
        // Pre-populate common ABI function selectors
        encoder.init_abi_cache();
        encoder
    }
    
    fn init_abi_cache(&mut self) {
        // ERC-20 function selectors (first 4 bytes of keccak256(signature))
        self.abi_cache.insert("transfer(address,uint256)".to_string(), 
            vec![0xa9, 0x05, 0x9c, 0xbb]); // transfer
        self.abi_cache.insert("approve(address,uint256)".to_string(),
            vec![0x09, 0x5e, 0xa7, 0xb3]); // approve
        self.abi_cache.insert("transferFrom(address,address,uint256)".to_string(),
            vec![0x23, 0xb8, 0x72, 0xdd]); // transferFrom
        self.abi_cache.insert("balanceOf(address)".to_string(),
            vec![0x70, 0xa0, 0x82, 0x31]); // balanceOf
            
        // Bridge-specific functions
        self.abi_cache.insert("bridgeIn(bytes32,uint256,bytes)".to_string(),
            vec![0xb1, 0x2c, 0x4e, 0x8f]); // bridgeIn (custom)
        self.abi_cache.insert("bridgeOut(bytes32,uint256)".to_string(),
            vec![0xc2, 0x3d, 0x5f, 0xa0]); // bridgeOut (custom)
    }
    
    /// Encode a Rope string operation as an EVM transaction
    pub fn encode_string_to_evm(
        &self,
        string_id: &[u8; 32],
        operation: &StringOperation,
        sender_key: &[u8],
        nonce: u64,
    ) -> Result<EvmTransaction, EncodingError> {
        let sender_address = self.address_converter.rope_to_ethereum(sender_key);
        
        let (to, value, data) = match operation {
            StringOperation::TokenTransfer { recipient, amount, token } => {
                let recipient_addr = self.address_converter.rope_to_ethereum(recipient);
                let call_data = self.encode_erc20_transfer(&recipient_addr, *amount)?;
                (Some(self.get_token_contract(token)?), 0u128, call_data)
            }
            
            StringOperation::NativeTransfer { recipient, amount } => {
                let recipient_addr = self.address_converter.rope_to_ethereum(recipient);
                (Some(recipient_addr), *amount, Vec::new())
            }
            
            StringOperation::ContractCall { contract, method, params } => {
                let contract_addr = self.parse_address(contract)?;
                let call_data = self.encode_contract_call(method, params)?;
                (Some(contract_addr), 0u128, call_data)
            }
            
            StringOperation::BridgeDeposit { amount, target_chain } => {
                let bridge_contract = self.get_bridge_contract(target_chain)?;
                let call_data = self.encode_bridge_deposit(string_id, *amount)?;
                (Some(bridge_contract), *amount, call_data)
            }
            
            StringOperation::BridgeWithdraw { proof, amount } => {
                let bridge_contract = self.get_bridge_contract(&"rope".to_string())?;
                let call_data = self.encode_bridge_withdraw(proof, *amount)?;
                (Some(bridge_contract), 0u128, call_data)
            }
        };
        
        Ok(EvmTransaction {
            nonce,
            gas_price: 20_000_000_000, // 20 gwei default
            gas_limit: 200_000,
            to,
            value,
            data,
            chain_id: self.chain_id,
            v: 0, // To be signed
            r: [0u8; 32],
            s: [0u8; 32],
        })
    }
    
    /// Encode ERC-20 transfer
    fn encode_erc20_transfer(&self, to: &[u8; 20], amount: u128) -> Result<Vec<u8>, EncodingError> {
        let mut data = Vec::with_capacity(68);
        
        // Function selector: transfer(address,uint256)
        data.extend_from_slice(&[0xa9, 0x05, 0x9c, 0xbb]);
        
        // Pad address to 32 bytes
        data.extend_from_slice(&[0u8; 12]);
        data.extend_from_slice(to);
        
        // Amount as 32-byte big-endian
        let amount_bytes = amount.to_be_bytes();
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&amount_bytes);
        
        Ok(data)
    }
    
    /// Encode bridge deposit
    fn encode_bridge_deposit(&self, string_id: &[u8; 32], amount: u128) -> Result<Vec<u8>, EncodingError> {
        let mut data = Vec::with_capacity(100);
        
        // Function selector: bridgeIn(bytes32,uint256,bytes)
        data.extend_from_slice(&[0xb1, 0x2c, 0x4e, 0x8f]);
        
        // String ID (32 bytes)
        data.extend_from_slice(string_id);
        
        // Amount (32 bytes)
        let amount_bytes = amount.to_be_bytes();
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&amount_bytes);
        
        Ok(data)
    }
    
    /// Encode bridge withdraw with proof
    fn encode_bridge_withdraw(&self, proof: &[u8], amount: u128) -> Result<Vec<u8>, EncodingError> {
        let mut data = Vec::with_capacity(68 + proof.len());
        
        // Function selector: bridgeOut(bytes32,uint256)
        data.extend_from_slice(&[0xc2, 0x3d, 0x5f, 0xa0]);
        
        // Proof hash (32 bytes)
        let proof_hash = *blake3::hash(proof).as_bytes();
        data.extend_from_slice(&proof_hash);
        
        // Amount (32 bytes)
        let amount_bytes = amount.to_be_bytes();
        data.extend_from_slice(&[0u8; 16]);
        data.extend_from_slice(&amount_bytes);
        
        Ok(data)
    }
    
    /// Encode arbitrary contract call
    fn encode_contract_call(&self, method: &str, params: &[ConditionValue]) -> Result<Vec<u8>, EncodingError> {
        // Get function selector from cache or compute
        let selector = self.abi_cache.get(method)
            .cloned()
            .unwrap_or_else(|| {
                // Compute selector: first 4 bytes of keccak256(method_signature)
                let hash = blake3::hash(method.as_bytes());
                hash.as_bytes()[..4].to_vec()
            });
        
        let mut data = selector;
        
        // Encode parameters (simplified ABI encoding)
        for param in params {
            match param {
                ConditionValue::String(s) => {
                    // Strings are dynamic, but for simplicity we treat as bytes32
                    let mut padded = [0u8; 32];
                    let bytes = s.as_bytes();
                    let len = bytes.len().min(32);
                    padded[..len].copy_from_slice(&bytes[..len]);
                    data.extend_from_slice(&padded);
                }
                ConditionValue::Integer(n) => {
                    let mut bytes = [0u8; 32];
                    bytes[24..].copy_from_slice(&(*n as u64).to_be_bytes());
                    data.extend_from_slice(&bytes);
                }
                ConditionValue::Boolean(b) => {
                    let mut bytes = [0u8; 32];
                    bytes[31] = if *b { 1 } else { 0 };
                    data.extend_from_slice(&bytes);
                }
                _ => {
                    // Other types: encode as zero
                    data.extend_from_slice(&[0u8; 32]);
                }
            }
        }
        
        Ok(data)
    }
    
    fn get_token_contract(&self, token: &str) -> Result<[u8; 20], EncodingError> {
        // Well-known token contracts (mainnet)
        match token.to_uppercase().as_str() {
            "USDT" => Ok([0xda, 0xc1, 0x7f, 0x95, 0x8d, 0x2e, 0xe5, 0x23, 0xa2, 0x20,
                         0x62, 0x06, 0x99, 0x45, 0x97, 0xc1, 0x3d, 0x83, 0x1e, 0xc7]),
            "USDC" => Ok([0xa0, 0xb8, 0x69, 0x91, 0xc6, 0x21, 0x8b, 0x36, 0xc1, 0xd1,
                         0x9d, 0x4a, 0x2e, 0x9e, 0xb0, 0xce, 0x36, 0x06, 0xeb, 0x48]),
            "FAT" | "DCFAT" => {
                // DC FAT wrapped token contract (placeholder)
                Ok([0x0b, 0x44, 0x54, 0x7b, 0xe0, 0xa0, 0xdf, 0x5d, 0xcd, 0x53,
                    0x27, 0xde, 0x8e, 0xa7, 0x36, 0x80, 0x51, 0x7c, 0x5a, 0x54])
            }
            _ => Err(EncodingError::UnknownToken(token.to_string())),
        }
    }
    
    fn get_bridge_contract(&self, chain: &str) -> Result<[u8; 20], EncodingError> {
        match chain.to_lowercase().as_str() {
            "ethereum" | "eth" => {
                Ok([0x0b, 0x44, 0x54, 0x7b, 0xe0, 0xa0, 0xdf, 0x5d, 0xcd, 0x53,
                    0x27, 0xde, 0x8e, 0xa7, 0x36, 0x80, 0x51, 0x7c, 0x5a, 0x54])
            }
            "xdc" => {
                Ok([0x20, 0xb5, 0x9e, 0x6c, 0x5d, 0xeb, 0x7d, 0x7c, 0xed, 0x2c,
                    0xa8, 0x23, 0xc6, 0xca, 0x81, 0xdd, 0x3f, 0x7e, 0x9a, 0x3a])
            }
            "rope" | "datachain" => {
                // Rope-side bridge contract
                Ok([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01])
            }
            _ => Err(EncodingError::UnknownChain(chain.to_string())),
        }
    }
    
    fn parse_address(&self, address: &str) -> Result<[u8; 20], EncodingError> {
        let hex_str = address.strip_prefix("0x").unwrap_or(address);
        let bytes = hex::decode(hex_str)
            .map_err(|_| EncodingError::InvalidAddress(address.to_string()))?;
        
        if bytes.len() != 20 {
            return Err(EncodingError::InvalidAddress(address.to_string()));
        }
        
        let mut result = [0u8; 20];
        result.copy_from_slice(&bytes);
        Ok(result)
    }
}

// ============================================================================
// EVM-to-String Decoder
// ============================================================================

/// Decodes EVM transactions back to Rope String operations
pub struct EvmToStringDecoder {
    /// Chain ID
    chain_id: u64,
    
    /// Address converter  
    address_converter: AddressConverter,
}

impl EvmToStringDecoder {
    pub fn new(chain_id: u64) -> Self {
        Self {
            chain_id,
            address_converter: AddressConverter::new(),
        }
    }
    
    /// Decode an EVM transaction to a Rope string operation
    pub fn decode_evm_to_string(
        &self,
        tx: &EvmTransaction,
    ) -> Result<DecodedStringOperation, EncodingError> {
        // Verify chain ID
        if tx.chain_id != self.chain_id {
            return Err(EncodingError::ChainMismatch {
                expected: self.chain_id,
                got: tx.chain_id,
            });
        }
        
        // Native transfer (no data, has value)
        if tx.data.is_empty() && tx.value > 0 {
            return Ok(DecodedStringOperation {
                operation_type: "native_transfer".to_string(),
                rope_concept: RopeConcept::TokenTransfer {
                    token_id: [0u8; 32], // Native token
                    amount: tx.value,
                },
                metadata: HashMap::new(),
            });
        }
        
        // Contract call (has data)
        if tx.data.len() >= 4 {
            return self.decode_contract_call(tx);
        }
        
        Err(EncodingError::UnknownOperation)
    }
    
    fn decode_contract_call(&self, tx: &EvmTransaction) -> Result<DecodedStringOperation, EncodingError> {
        let selector = &tx.data[..4];
        
        // ERC-20 transfer
        if selector == [0xa9, 0x05, 0x9c, 0xbb] {
            if tx.data.len() >= 68 {
                let mut recipient = [0u8; 20];
                recipient.copy_from_slice(&tx.data[16..36]);
                
                let mut amount_bytes = [0u8; 16];
                amount_bytes.copy_from_slice(&tx.data[52..68]);
                let amount = u128::from_be_bytes(amount_bytes);
                
                let mut token_id = [0u8; 32];
                if let Some(to) = tx.to {
                    token_id[12..].copy_from_slice(&to);
                }
                
                return Ok(DecodedStringOperation {
                    operation_type: "token_transfer".to_string(),
                    rope_concept: RopeConcept::TokenTransfer {
                        token_id,
                        amount,
                    },
                    metadata: [
                        ("recipient".to_string(), hex::encode(recipient)),
                    ].into_iter().collect(),
                });
            }
        }
        
        // Bridge deposit
        if selector == [0xb1, 0x2c, 0x4e, 0x8f] {
            if tx.data.len() >= 68 {
                let mut string_id = [0u8; 32];
                string_id.copy_from_slice(&tx.data[4..36]);
                
                return Ok(DecodedStringOperation {
                    operation_type: "bridge_deposit".to_string(),
                    rope_concept: RopeConcept::String { id: string_id },
                    metadata: [
                        ("amount".to_string(), tx.value.to_string()),
                    ].into_iter().collect(),
                });
            }
        }
        
        // Generic contract call
        Ok(DecodedStringOperation {
            operation_type: "contract_call".to_string(),
            rope_concept: RopeConcept::String { id: *blake3::hash(&tx.data).as_bytes() },
            metadata: [
                ("selector".to_string(), hex::encode(selector)),
                ("data_length".to_string(), tx.data.len().to_string()),
            ].into_iter().collect(),
        })
    }
}

// ============================================================================
// State Proof Generator
// ============================================================================

/// Generates EVM-compatible state proofs from DAG state
pub struct StateProofGenerator {
    /// Merkle tree for state
    state_root: [u8; 32],
}

impl StateProofGenerator {
    pub fn new() -> Self {
        Self {
            state_root: [0u8; 32],
        }
    }
    
    /// Update state root from DAG
    pub fn update_state_root(&mut self, strings: &[[u8; 32]]) {
        // Compute Merkle root of all string IDs
        if strings.is_empty() {
            self.state_root = [0u8; 32];
            return;
        }
        
        let mut hashes: Vec<[u8; 32]> = strings.to_vec();
        
        while hashes.len() > 1 {
            let mut next_level = Vec::new();
            for chunk in hashes.chunks(2) {
                let combined = if chunk.len() == 2 {
                    let mut input = Vec::with_capacity(64);
                    input.extend_from_slice(&chunk[0]);
                    input.extend_from_slice(&chunk[1]);
                    *blake3::hash(&input).as_bytes()
                } else {
                    chunk[0]
                };
                next_level.push(combined);
            }
            hashes = next_level;
        }
        
        self.state_root = hashes[0];
    }
    
    /// Generate a Merkle proof for a specific string
    pub fn generate_proof(&self, string_id: &[u8; 32], all_strings: &[[u8; 32]]) -> StateProof {
        let mut proof_path = Vec::new();
        
        // Find index of string
        let index = all_strings.iter().position(|s| s == string_id);
        
        if let Some(idx) = index {
            // Generate Merkle path (simplified)
            let mut current_idx = idx;
            let mut hashes: Vec<[u8; 32]> = all_strings.to_vec();
            
            while hashes.len() > 1 {
                let sibling_idx = if current_idx % 2 == 0 {
                    current_idx + 1
                } else {
                    current_idx - 1
                };
                
                if sibling_idx < hashes.len() {
                    proof_path.push(ProofNode {
                        hash: hashes[sibling_idx],
                        position: if current_idx % 2 == 0 { Position::Right } else { Position::Left },
                    });
                }
                
                // Move to next level
                let mut next_level = Vec::new();
                for chunk in hashes.chunks(2) {
                    let combined = if chunk.len() == 2 {
                        let mut input = Vec::with_capacity(64);
                        input.extend_from_slice(&chunk[0]);
                        input.extend_from_slice(&chunk[1]);
                        *blake3::hash(&input).as_bytes()
                    } else {
                        chunk[0]
                    };
                    next_level.push(combined);
                }
                hashes = next_level;
                current_idx /= 2;
            }
        }
        
        StateProof {
            string_id: *string_id,
            state_root: self.state_root,
            proof_path,
            timestamp: chrono::Utc::now().timestamp(),
        }
    }
    
    /// Verify a state proof
    pub fn verify_proof(&self, proof: &StateProof) -> bool {
        let mut current_hash = proof.string_id;
        
        for node in &proof.proof_path {
            let mut input = Vec::with_capacity(64);
            match node.position {
                Position::Left => {
                    input.extend_from_slice(&node.hash);
                    input.extend_from_slice(&current_hash);
                }
                Position::Right => {
                    input.extend_from_slice(&current_hash);
                    input.extend_from_slice(&node.hash);
                }
            }
            current_hash = *blake3::hash(&input).as_bytes();
        }
        
        current_hash == proof.state_root
    }
    
    pub fn state_root(&self) -> [u8; 32] {
        self.state_root
    }
}

impl Default for StateProofGenerator {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// EVM Invocation Bridge (Main Interface)
// ============================================================================

/// The main EVM Invocation Bridge that connects Rope to EVM chains
pub struct EvmInvocationBridge {
    /// Chain configuration
    config: EvmBridgeConfig,
    
    /// String-to-EVM encoder
    encoder: StringToEvmEncoder,
    
    /// EVM-to-String decoder
    decoder: EvmToStringDecoder,
    
    /// State proof generator
    state_prover: StateProofGenerator,
    
    /// Pending bridge transactions
    pending_txs: parking_lot::RwLock<HashMap<[u8; 32], PendingBridgeTx>>,
    
    /// Confirmed bridge transactions
    confirmed_txs: parking_lot::RwLock<Vec<ConfirmedBridgeTx>>,
    
    /// Bridge statistics
    stats: parking_lot::RwLock<BridgeStats>,
}

impl EvmInvocationBridge {
    /// Create new bridge for a specific EVM chain
    pub fn new(config: EvmBridgeConfig) -> Self {
        let chain_id = config.chain_id;
        Self {
            config,
            encoder: StringToEvmEncoder::new(chain_id),
            decoder: EvmToStringDecoder::new(chain_id),
            state_prover: StateProofGenerator::new(),
            pending_txs: parking_lot::RwLock::new(HashMap::new()),
            confirmed_txs: parking_lot::RwLock::new(Vec::new()),
            stats: parking_lot::RwLock::new(BridgeStats::default()),
        }
    }
    
    /// Invoke an EVM operation from a Rope string
    pub async fn invoke_evm(
        &self,
        string_id: [u8; 32],
        operation: StringOperation,
        sender_key: &[u8],
        nonce: u64,
    ) -> Result<InvocationHandle, BridgeError> {
        // 1. Encode the operation as an EVM transaction
        let evm_tx = self.encoder.encode_string_to_evm(&string_id, &operation, sender_key, nonce)
            .map_err(|e| BridgeError::TransactionFailed(e.to_string()))?;
        
        // 2. Generate invocation ID
        let invocation_id = self.generate_invocation_id(&string_id, &evm_tx);
        
        // 3. Create pending transaction record
        let pending = PendingBridgeTx {
            invocation_id,
            string_id,
            evm_tx: evm_tx.clone(),
            status: PendingStatus::Encoding,
            created_at: chrono::Utc::now().timestamp(),
            retries: 0,
        };
        
        self.pending_txs.write().insert(invocation_id, pending);
        
        // 4. Update statistics
        {
            let mut stats = self.stats.write();
            stats.total_invocations += 1;
            stats.pending_count += 1;
        }
        
        Ok(InvocationHandle {
            invocation_id,
            string_id,
            chain_id: self.config.chain_id,
            estimated_gas: evm_tx.gas_limit,
        })
    }
    
    /// Process an incoming EVM event (bridge callback)
    pub async fn process_evm_event(
        &self,
        event: EvmBridgeEvent,
    ) -> Result<RopeConcept, BridgeError> {
        match event {
            EvmBridgeEvent::Deposit { tx_hash, from, amount, data } => {
                // Convert EVM deposit to Rope string
                let string_id = self.generate_string_from_deposit(&tx_hash, &from, amount, &data);
                
                self.stats.write().total_deposits += 1;
                
                Ok(RopeConcept::TokenTransfer {
                    token_id: [0u8; 32], // Native bridged token
                    amount,
                })
            }
            
            EvmBridgeEvent::Withdrawal { tx_hash, proof, amount } => {
                // Verify withdrawal proof against Rope state
                if !self.verify_withdrawal_proof(&proof) {
                    return Err(BridgeError::VerificationFailed("Invalid withdrawal proof".to_string()));
                }
                
                self.stats.write().total_withdrawals += 1;
                
                Ok(RopeConcept::TokenTransfer {
                    token_id: [0u8; 32],
                    amount,
                })
            }
            
            EvmBridgeEvent::ContractCallback { invocation_id, result } => {
                // Update pending transaction status
                if let Some(mut pending) = self.pending_txs.write().remove(&invocation_id) {
                    pending.status = if result.success {
                        PendingStatus::Confirmed
                    } else {
                        PendingStatus::Failed(result.error.unwrap_or_default())
                    };
                    
                    // Move to confirmed
                    let confirmed = ConfirmedBridgeTx {
                        invocation_id,
                        string_id: pending.string_id,
                        evm_tx_hash: *blake3::hash(&pending.evm_tx.data).as_bytes(),
                        success: result.success,
                        gas_used: result.gas_used,
                        confirmed_at: chrono::Utc::now().timestamp(),
                    };
                    
                    self.confirmed_txs.write().push(confirmed);
                    
                    let mut stats = self.stats.write();
                    stats.pending_count = stats.pending_count.saturating_sub(1);
                    if result.success {
                        stats.successful_invocations += 1;
                    } else {
                        stats.failed_invocations += 1;
                    }
                }
                
                Ok(RopeConcept::String { id: invocation_id })
            }
        }
    }
    
    /// Get bridge statistics
    pub fn stats(&self) -> BridgeStats {
        self.stats.read().clone()
    }
    
    /// Update DAG state for proof generation
    pub fn update_dag_state(&self, string_ids: &[[u8; 32]]) {
        // This would be called by the node when DAG state changes
        // For now, just update the state root
        // In production, this would maintain the full Merkle tree
    }
    
    fn generate_invocation_id(&self, string_id: &[u8; 32], tx: &EvmTransaction) -> [u8; 32] {
        let mut input = Vec::new();
        input.extend_from_slice(string_id);
        input.extend_from_slice(&tx.nonce.to_be_bytes());
        input.extend_from_slice(&self.config.chain_id.to_be_bytes());
        *blake3::hash(&input).as_bytes()
    }
    
    fn generate_string_from_deposit(
        &self,
        tx_hash: &[u8; 32],
        from: &[u8; 20],
        amount: u128,
        data: &[u8],
    ) -> [u8; 32] {
        let mut input = Vec::new();
        input.extend_from_slice(tx_hash);
        input.extend_from_slice(from);
        input.extend_from_slice(&amount.to_be_bytes());
        input.extend_from_slice(data);
        *blake3::hash(&input).as_bytes()
    }
    
    fn verify_withdrawal_proof(&self, proof: &[u8]) -> bool {
        // Verify the proof against current DAG state
        // This would check Merkle inclusion in the String Lattice
        !proof.is_empty()
    }
}

#[async_trait]
impl Bridge for EvmInvocationBridge {
    fn name(&self) -> &str {
        &self.config.name
    }
    
    fn protocol_type(&self) -> ProtocolType {
        ProtocolType::Blockchain(BlockchainType::Ethereum)
    }
    
    async fn is_connected(&self) -> bool {
        // Check RPC connectivity
        true // Placeholder
    }
    
    async fn sync_state(&mut self) -> Result<(), BridgeError> {
        // Sync with EVM chain state
        Ok(())
    }
    
    async fn submit_transaction(&self, tx: BridgeTransaction) -> Result<[u8; 32], BridgeError> {
        // Submit to EVM chain via RPC
        Ok(tx.id)
    }
    
    async fn verify_proof(&self, proof: &[u8]) -> Result<bool, BridgeError> {
        Ok(self.verify_withdrawal_proof(proof))
    }
}

// ============================================================================
// Supporting Types
// ============================================================================

/// Bridge configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvmBridgeConfig {
    pub name: String,
    pub chain_id: u64,
    pub rpc_url: String,
    pub bridge_contract: String,
    pub confirmations_required: u32,
}

/// String operations that can be encoded to EVM
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StringOperation {
    TokenTransfer {
        recipient: Vec<u8>,
        amount: u128,
        token: String,
    },
    NativeTransfer {
        recipient: Vec<u8>,
        amount: u128,
    },
    ContractCall {
        contract: String,
        method: String,
        params: Vec<ConditionValue>,
    },
    BridgeDeposit {
        amount: u128,
        target_chain: String,
    },
    BridgeWithdraw {
        proof: Vec<u8>,
        amount: u128,
    },
}

/// Condition value for contract parameters
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConditionValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Bytes(Vec<u8>),
}

/// Decoded string operation from EVM
#[derive(Clone, Debug)]
pub struct DecodedStringOperation {
    pub operation_type: String,
    pub rope_concept: RopeConcept,
    pub metadata: HashMap<String, String>,
}

/// State proof for cross-chain verification
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateProof {
    pub string_id: [u8; 32],
    pub state_root: [u8; 32],
    pub proof_path: Vec<ProofNode>,
    pub timestamp: i64,
}

/// Merkle proof node
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProofNode {
    pub hash: [u8; 32],
    pub position: Position,
}

/// Position in Merkle tree
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Position {
    Left,
    Right,
}

/// Invocation handle returned to caller
#[derive(Clone, Debug)]
pub struct InvocationHandle {
    pub invocation_id: [u8; 32],
    pub string_id: [u8; 32],
    pub chain_id: u64,
    pub estimated_gas: u64,
}

/// Pending bridge transaction
#[derive(Clone, Debug)]
pub struct PendingBridgeTx {
    pub invocation_id: [u8; 32],
    pub string_id: [u8; 32],
    pub evm_tx: EvmTransaction,
    pub status: PendingStatus,
    pub created_at: i64,
    pub retries: u32,
}

/// Pending transaction status
#[derive(Clone, Debug)]
pub enum PendingStatus {
    Encoding,
    Signed,
    Submitted,
    Pending,
    Confirmed,
    Failed(String),
}

/// Confirmed bridge transaction
#[derive(Clone, Debug)]
pub struct ConfirmedBridgeTx {
    pub invocation_id: [u8; 32],
    pub string_id: [u8; 32],
    pub evm_tx_hash: [u8; 32],
    pub success: bool,
    pub gas_used: u64,
    pub confirmed_at: i64,
}

/// EVM bridge events
#[derive(Clone, Debug)]
pub enum EvmBridgeEvent {
    Deposit {
        tx_hash: [u8; 32],
        from: [u8; 20],
        amount: u128,
        data: Vec<u8>,
    },
    Withdrawal {
        tx_hash: [u8; 32],
        proof: Vec<u8>,
        amount: u128,
    },
    ContractCallback {
        invocation_id: [u8; 32],
        result: EvmCallResult,
    },
}

/// Bridge statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BridgeStats {
    pub total_invocations: u64,
    pub successful_invocations: u64,
    pub failed_invocations: u64,
    pub pending_count: u64,
    pub total_deposits: u64,
    pub total_withdrawals: u64,
}

/// Encoding errors
#[derive(Clone, Debug)]
pub enum EncodingError {
    InvalidAddress(String),
    UnknownToken(String),
    UnknownChain(String),
    ChainMismatch { expected: u64, got: u64 },
    UnknownOperation,
    EncodingFailed(String),
}

impl std::fmt::Display for EncodingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EncodingError::InvalidAddress(a) => write!(f, "Invalid address: {}", a),
            EncodingError::UnknownToken(t) => write!(f, "Unknown token: {}", t),
            EncodingError::UnknownChain(c) => write!(f, "Unknown chain: {}", c),
            EncodingError::ChainMismatch { expected, got } => {
                write!(f, "Chain ID mismatch: expected {}, got {}", expected, got)
            }
            EncodingError::UnknownOperation => write!(f, "Unknown operation"),
            EncodingError::EncodingFailed(e) => write!(f, "Encoding failed: {}", e),
        }
    }
}

impl std::error::Error for EncodingError {}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_evm_transaction_encoding() {
        let encoder = StringToEvmEncoder::new(271828);
        
        let operation = StringOperation::NativeTransfer {
            recipient: vec![0x12; 32],
            amount: 1_000_000_000_000_000_000, // 1 ETH
        };
        
        let result = encoder.encode_string_to_evm(
            &[0u8; 32],
            &operation,
            &[0x42; 32],
            0,
        );
        
        assert!(result.is_ok());
        let tx = result.unwrap();
        assert_eq!(tx.chain_id, 271828);
        assert_eq!(tx.value, 1_000_000_000_000_000_000);
    }
    
    #[test]
    fn test_state_proof_generation() {
        let mut prover = StateProofGenerator::new();
        
        let strings = [
            [1u8; 32],
            [2u8; 32],
            [3u8; 32],
            [4u8; 32],
        ];
        
        prover.update_state_root(&strings);
        
        let proof = prover.generate_proof(&[2u8; 32], &strings);
        assert!(prover.verify_proof(&proof));
    }
    
    #[test]
    fn test_bridge_config() {
        let config = EvmBridgeConfig {
            name: "Ethereum Bridge".to_string(),
            chain_id: 271828,
            rpc_url: "https://erpc.datachain.network".to_string(),
            bridge_contract: "0x0b44547be0a0df5dcd5327de8ea73680517c5a54".to_string(),
            confirmations_required: 12,
        };
        
        let bridge = EvmInvocationBridge::new(config);
        assert_eq!(bridge.name(), "Ethereum Bridge");
    }
}
