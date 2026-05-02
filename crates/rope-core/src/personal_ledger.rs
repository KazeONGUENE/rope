//! # Personal Ledger — One String Per Wallet
//!
//! Implements the personal ledger model where each wallet address maps to
//! exactly one logical String that grows over time as the wallet interacts
//! with third parties on the Datachain Rope network.
//!
//! ## Model
//!
//! ```text
//! Wallet W₁ ──► LedgerChain { genesis_id, head_id, entries: [S₁ → S₂ → S₃ → ...] }
//! Wallet W₂ ──► LedgerChain { genesis_id, head_id, entries: [S₁ → S₂ → ...] }
//! ```
//!
//! Each LedgerEntry is a `RopeString` where:
//! - `parentage` (π) points to the previous entry in the chain
//! - `sequence` (σ) contains the encrypted interaction data (via LedgerEnvelope)
//! - `mutability_class` (μ) is `GDPRCompliant` or `OwnerErasable`
//! - `oes_generation` marks which OES epoch encrypted the content
//!
//! The chain is an append-only linked list within the DAG lattice.
//! Repatriation fetches all entries and reconstructs the full ledger.
//! Deletion destroys the OES key, making all entries unreadable.

use crate::clock::LamportClock;
use crate::string::{HybridSignature, OESProof, PublicKey, RopeString};
use crate::types::{MutabilityClass, NodeId, StringId};
use hashbrown::HashMap;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

/// Metadata describing a personal ledger's current state.
/// Stored in the ledger registry — does NOT contain the actual data.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerDescriptor {
    pub wallet_address: Vec<u8>,
    pub genesis_string_id: StringId,
    pub head_string_id: StringId,
    pub entry_count: u64,
    pub total_size_bytes: u64,
    pub oes_generation_at_creation: u64,
    pub current_oes_generation: u64,
    pub created_at: i64,
    pub last_appended_at: i64,
    pub is_deleted: bool,
    pub deleted_at: Option<i64>,
    pub piece_count: u32,
    pub replication_factor: u32,
}

impl LedgerDescriptor {
    pub fn wallet_hex(&self) -> String {
        format!("0x{}", hex::encode(&self.wallet_address))
    }
}

/// A single interaction record before encryption.
/// This is what the wallet submits to append to its ledger.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InteractionRecord {
    pub interaction_type: InteractionType,
    pub counterparty: Option<Vec<u8>>,
    pub data: Vec<u8>,
    pub timestamp: i64,
    pub metadata: HashMap<String, String>,
}

/// Types of interactions that get recorded in a personal ledger
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InteractionType {
    Transfer,
    ContractCall,
    TokenApproval,
    IdentityClaim,
    TestimonySubmission,
    DataSharing,
    StakeDeposit,
    StakeWithdraw,
    BridgeOperation,
    Custom(String),
}

impl InteractionType {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Transfer => "transfer",
            Self::ContractCall => "contract_call",
            Self::TokenApproval => "token_approval",
            Self::IdentityClaim => "identity_claim",
            Self::TestimonySubmission => "testimony_submission",
            Self::DataSharing => "data_sharing",
            Self::StakeDeposit => "stake_deposit",
            Self::StakeWithdraw => "stake_withdraw",
            Self::BridgeOperation => "bridge_operation",
            Self::Custom(name) => name,
        }
    }
}

/// Piece map entry — records where each RDP piece of a ledger entry is stored
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PieceLocation {
    pub piece_index: u32,
    pub piece_hash: [u8; 32],
    pub piece_size: u32,
    pub holders: Vec<NodeId>,
}

/// Full piece map for a single ledger entry (one RopeString)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntryPieceMap {
    pub string_id: StringId,
    pub total_pieces: u32,
    pub total_size: u64,
    pub pieces: Vec<PieceLocation>,
}

impl EntryPieceMap {
    pub fn new(string_id: StringId) -> Self {
        Self {
            string_id,
            total_pieces: 0,
            total_size: 0,
            pieces: Vec::new(),
        }
    }

    pub fn add_piece(&mut self, index: u32, hash: [u8; 32], size: u32, holders: Vec<NodeId>) {
        self.pieces.push(PieceLocation {
            piece_index: index,
            piece_hash: hash,
            piece_size: size,
            holders,
        });
        self.total_pieces = self.pieces.len() as u32;
        self.total_size += size as u64;
    }

    pub fn is_complete(&self) -> bool {
        !self.pieces.is_empty()
            && self.pieces.len() as u32 == self.total_pieces
            && self.pieces.iter().all(|p| !p.holders.is_empty())
    }

    pub fn missing_pieces(&self) -> Vec<u32> {
        let have: std::collections::HashSet<u32> =
            self.pieces.iter().map(|p| p.piece_index).collect();
        (0..self.total_pieces)
            .filter(|i| !have.contains(i))
            .collect()
    }
}

/// The ledger chain — a linked list of RopeString IDs forming one wallet's ledger.
/// This is the in-memory representation used during repatriation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerChain {
    pub descriptor: LedgerDescriptor,
    pub entry_ids: Vec<StringId>,
    pub piece_maps: HashMap<StringId, EntryPieceMap>,
}

impl LedgerChain {
    pub fn new(descriptor: LedgerDescriptor) -> Self {
        Self {
            entry_ids: Vec::new(),
            piece_maps: HashMap::new(),
            descriptor,
        }
    }

    pub fn append_entry_id(&mut self, id: StringId) {
        self.entry_ids.push(id);
        self.descriptor.head_string_id = id;
        self.descriptor.entry_count = self.entry_ids.len() as u64;
        self.descriptor.last_appended_at = chrono::Utc::now().timestamp();
    }

    pub fn set_piece_map(&mut self, id: StringId, map: EntryPieceMap) {
        self.descriptor.piece_count += map.total_pieces;
        self.piece_maps.insert(id, map);
    }

    pub fn head(&self) -> StringId {
        self.descriptor.head_string_id
    }

    pub fn genesis(&self) -> StringId {
        self.descriptor.genesis_string_id
    }

    pub fn len(&self) -> usize {
        self.entry_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entry_ids.is_empty()
    }
}

/// Registry mapping wallet addresses to their ledger descriptors.
/// Thread-safe for concurrent node operation.
pub struct LedgerRegistry {
    ledgers: RwLock<HashMap<Vec<u8>, LedgerDescriptor>>,
    wallet_to_genesis: RwLock<HashMap<Vec<u8>, StringId>>,
    wallet_to_head: RwLock<HashMap<Vec<u8>, StringId>>,
    string_to_wallet: RwLock<HashMap<StringId, Vec<u8>>>,
}

impl LedgerRegistry {
    pub fn new() -> Self {
        Self {
            ledgers: RwLock::new(HashMap::new()),
            wallet_to_genesis: RwLock::new(HashMap::new()),
            wallet_to_head: RwLock::new(HashMap::new()),
            string_to_wallet: RwLock::new(HashMap::new()),
        }
    }

    /// Register a new personal ledger for a wallet.
    /// Returns the genesis StringId.
    pub fn create_ledger(
        &self,
        wallet_address: &[u8],
        genesis_id: StringId,
        oes_generation: u64,
        replication_factor: u32,
    ) -> Result<LedgerDescriptor, LedgerRegistryError> {
        let mut ledgers = self.ledgers.write();
        if ledgers.contains_key(wallet_address) {
            return Err(LedgerRegistryError::AlreadyExists(hex::encode(
                wallet_address,
            )));
        }

        let now = chrono::Utc::now().timestamp();
        let descriptor = LedgerDescriptor {
            wallet_address: wallet_address.to_vec(),
            genesis_string_id: genesis_id,
            head_string_id: genesis_id,
            entry_count: 1,
            total_size_bytes: 0,
            oes_generation_at_creation: oes_generation,
            current_oes_generation: oes_generation,
            created_at: now,
            last_appended_at: now,
            is_deleted: false,
            deleted_at: None,
            piece_count: 0,
            replication_factor,
        };

        ledgers.insert(wallet_address.to_vec(), descriptor.clone());
        self.wallet_to_genesis
            .write()
            .insert(wallet_address.to_vec(), genesis_id);
        self.wallet_to_head
            .write()
            .insert(wallet_address.to_vec(), genesis_id);
        self.string_to_wallet
            .write()
            .insert(genesis_id, wallet_address.to_vec());

        Ok(descriptor)
    }

    /// Record a new entry appended to a wallet's ledger
    pub fn record_append(
        &self,
        wallet_address: &[u8],
        new_string_id: StringId,
        content_size: u64,
        oes_generation: u64,
    ) -> Result<(), LedgerRegistryError> {
        let mut ledgers = self.ledgers.write();
        let desc = ledgers
            .get_mut(wallet_address)
            .ok_or_else(|| LedgerRegistryError::NotFound(hex::encode(wallet_address)))?;

        if desc.is_deleted {
            return Err(LedgerRegistryError::Deleted(hex::encode(wallet_address)));
        }

        desc.head_string_id = new_string_id;
        desc.entry_count += 1;
        desc.total_size_bytes += content_size;
        desc.current_oes_generation = oes_generation;
        desc.last_appended_at = chrono::Utc::now().timestamp();

        self.wallet_to_head
            .write()
            .insert(wallet_address.to_vec(), new_string_id);
        self.string_to_wallet
            .write()
            .insert(new_string_id, wallet_address.to_vec());

        Ok(())
    }

    /// Mark a ledger as deleted (OES key destroyed)
    pub fn mark_deleted(&self, wallet_address: &[u8]) -> Result<(), LedgerRegistryError> {
        let mut ledgers = self.ledgers.write();
        let desc = ledgers
            .get_mut(wallet_address)
            .ok_or_else(|| LedgerRegistryError::NotFound(hex::encode(wallet_address)))?;

        desc.is_deleted = true;
        desc.deleted_at = Some(chrono::Utc::now().timestamp());

        Ok(())
    }

    /// Look up the ledger descriptor for a wallet
    pub fn get_descriptor(&self, wallet_address: &[u8]) -> Option<LedgerDescriptor> {
        self.ledgers.read().get(wallet_address).cloned()
    }

    /// Look up which wallet owns a given string
    pub fn wallet_for_string(&self, string_id: &StringId) -> Option<Vec<u8>> {
        self.string_to_wallet.read().get(string_id).cloned()
    }

    /// Get the head (latest) StringId for a wallet
    pub fn head_for_wallet(&self, wallet_address: &[u8]) -> Option<StringId> {
        self.wallet_to_head.read().get(wallet_address).copied()
    }

    /// Get the genesis StringId for a wallet
    pub fn genesis_for_wallet(&self, wallet_address: &[u8]) -> Option<StringId> {
        self.wallet_to_genesis.read().get(wallet_address).copied()
    }

    /// List all registered wallets
    pub fn all_wallets(&self) -> Vec<Vec<u8>> {
        self.ledgers.read().keys().cloned().collect()
    }

    /// Count of active (non-deleted) ledgers
    pub fn active_count(&self) -> usize {
        self.ledgers
            .read()
            .values()
            .filter(|d| !d.is_deleted)
            .count()
    }

    /// Count of total ledgers
    pub fn total_count(&self) -> usize {
        self.ledgers.read().len()
    }
}

impl Default for LedgerRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Helper: build the genesis RopeString for a new wallet ledger
pub fn build_genesis_string(
    wallet_address: &[u8],
    creator: PublicKey,
    clock: LamportClock,
    oes_generation: u64,
    oes_proof: OESProof,
    replication_factor: u32,
) -> Result<RopeString, &'static str> {
    let genesis_content = build_genesis_content(wallet_address);

    RopeString::builder()
        .content(genesis_content)
        .temporal_marker(clock)
        .creator(creator)
        .parentage(vec![StringId::ZERO])
        .replication_factor(replication_factor)
        .mutability_class(MutabilityClass::GDPRCompliant)
        .oes_generation(oes_generation)
        .oes_proof(oes_proof)
        .signature(HybridSignature::empty())
        .build()
}

/// Build deterministic genesis content for a wallet ledger
fn build_genesis_content(wallet_address: &[u8]) -> Vec<u8> {
    let mut content = Vec::new();
    content.extend_from_slice(b"LEDGER_GENESIS_V1:");
    content.extend_from_slice(wallet_address);
    content.extend_from_slice(&chrono::Utc::now().timestamp().to_le_bytes());
    content
}

/// Helper: build an append RopeString carrying encrypted interaction data
pub fn build_append_string(
    encrypted_envelope_bytes: Vec<u8>,
    parent_id: StringId,
    creator: PublicKey,
    clock: LamportClock,
    oes_generation: u64,
    oes_proof: OESProof,
    replication_factor: u32,
) -> Result<RopeString, &'static str> {
    RopeString::builder()
        .content(encrypted_envelope_bytes)
        .temporal_marker(clock)
        .creator(creator)
        .parentage(vec![parent_id])
        .replication_factor(replication_factor)
        .mutability_class(MutabilityClass::GDPRCompliant)
        .oes_generation(oes_generation)
        .oes_proof(oes_proof)
        .signature(HybridSignature::empty())
        .build()
}

/// Errors from the ledger registry
#[derive(Debug, Clone, thiserror::Error)]
pub enum LedgerRegistryError {
    #[error("ledger already exists for wallet {0}")]
    AlreadyExists(String),
    #[error("ledger not found for wallet {0}")]
    NotFound(String),
    #[error("ledger deleted for wallet {0}")]
    Deleted(String),
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_wallet() -> Vec<u8> {
        hex::decode("60FB32ef3A2381c2Ed71613F34fd56D56fCF4195").unwrap()
    }

    fn test_creator() -> PublicKey {
        PublicKey::from_ed25519([1u8; 32])
    }

    fn test_clock() -> LamportClock {
        LamportClock::new(NodeId::new([0u8; 32]))
    }

    #[test]
    fn test_ledger_registry_create() {
        let registry = LedgerRegistry::new();
        let wallet = test_wallet();
        let genesis_id = StringId::from_content(b"genesis");

        let desc = registry.create_ledger(&wallet, genesis_id, 0, 5).unwrap();
        assert_eq!(desc.entry_count, 1);
        assert_eq!(desc.genesis_string_id, genesis_id);
        assert_eq!(desc.head_string_id, genesis_id);
        assert!(!desc.is_deleted);
    }

    #[test]
    fn test_ledger_registry_duplicate_rejected() {
        let registry = LedgerRegistry::new();
        let wallet = test_wallet();
        let genesis_id = StringId::from_content(b"genesis");

        registry.create_ledger(&wallet, genesis_id, 0, 5).unwrap();
        let result = registry.create_ledger(&wallet, genesis_id, 0, 5);
        assert!(result.is_err());
    }

    #[test]
    fn test_ledger_registry_append() {
        let registry = LedgerRegistry::new();
        let wallet = test_wallet();
        let genesis_id = StringId::from_content(b"genesis");

        registry.create_ledger(&wallet, genesis_id, 0, 5).unwrap();

        let new_id = StringId::from_content(b"entry_1");
        registry.record_append(&wallet, new_id, 256, 1).unwrap();

        let desc = registry.get_descriptor(&wallet).unwrap();
        assert_eq!(desc.entry_count, 2);
        assert_eq!(desc.head_string_id, new_id);
        assert_eq!(desc.current_oes_generation, 1);
    }

    #[test]
    fn test_ledger_registry_delete() {
        let registry = LedgerRegistry::new();
        let wallet = test_wallet();
        let genesis_id = StringId::from_content(b"genesis");

        registry.create_ledger(&wallet, genesis_id, 0, 5).unwrap();
        registry.mark_deleted(&wallet).unwrap();

        let desc = registry.get_descriptor(&wallet).unwrap();
        assert!(desc.is_deleted);
        assert!(desc.deleted_at.is_some());
    }

    #[test]
    fn test_ledger_append_after_delete_fails() {
        let registry = LedgerRegistry::new();
        let wallet = test_wallet();
        let genesis_id = StringId::from_content(b"genesis");

        registry.create_ledger(&wallet, genesis_id, 0, 5).unwrap();
        registry.mark_deleted(&wallet).unwrap();

        let new_id = StringId::from_content(b"entry_1");
        let result = registry.record_append(&wallet, new_id, 256, 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_string_to_wallet_lookup() {
        let registry = LedgerRegistry::new();
        let wallet = test_wallet();
        let genesis_id = StringId::from_content(b"genesis");

        registry.create_ledger(&wallet, genesis_id, 0, 5).unwrap();

        let found = registry.wallet_for_string(&genesis_id);
        assert_eq!(found.unwrap(), wallet);
    }

    #[test]
    fn test_build_genesis_string() {
        let wallet = test_wallet();
        let string = build_genesis_string(
            &wallet,
            test_creator(),
            test_clock(),
            0,
            OESProof::empty(),
            5,
        )
        .unwrap();

        assert_eq!(string.replication_factor(), 5);
        assert_eq!(*string.mutability_class(), MutabilityClass::GDPRCompliant);
        assert_eq!(string.parentage(), &[StringId::ZERO]);
    }

    #[test]
    fn test_piece_map() {
        let id = StringId::from_content(b"test");
        let mut map = EntryPieceMap::new(id);
        let node = NodeId::new([1u8; 32]);

        map.add_piece(0, [0xAA; 32], 256 * 1024, vec![node]);
        map.add_piece(1, [0xBB; 32], 128 * 1024, vec![node]);

        assert_eq!(map.total_pieces, 2);
        assert!(map.is_complete());
        assert!(map.missing_pieces().is_empty());
    }

    #[test]
    fn test_ledger_chain() {
        let wallet = test_wallet();
        let genesis_id = StringId::from_content(b"genesis");
        let desc = LedgerDescriptor {
            wallet_address: wallet.clone(),
            genesis_string_id: genesis_id,
            head_string_id: genesis_id,
            entry_count: 1,
            total_size_bytes: 0,
            oes_generation_at_creation: 0,
            current_oes_generation: 0,
            created_at: 0,
            last_appended_at: 0,
            is_deleted: false,
            deleted_at: None,
            piece_count: 0,
            replication_factor: 5,
        };

        let mut chain = LedgerChain::new(desc);
        chain.append_entry_id(genesis_id);

        let entry_1 = StringId::from_content(b"entry_1");
        chain.append_entry_id(entry_1);

        assert_eq!(chain.len(), 2);
        assert_eq!(chain.head(), entry_1);
        assert_eq!(chain.genesis(), genesis_id);
    }
}
