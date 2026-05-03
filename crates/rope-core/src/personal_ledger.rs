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

/// What kind of entity does a string represent? Quipu Canon v1.2 model.
///
/// Each string is a logical, append-only chain of knots tied to exactly
/// one ecosystem entity. The registry indexes strings by `(kind, id)`
/// so two entities that share the same byte-string (e.g. an EVM wallet
/// address and a contract address that happen to collide) remain
/// distinct strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StringKind {
    /// EOA / hot wallet (default — keeps the v1.0/1.1 schema valid).
    #[default]
    Wallet,
    /// Smart contract deployed on Datachain Rope (DCSwap, T-REX, ONCHAINID,
    /// NaturaProof, Tanastok TREXFactory, etc.).
    Contract,
    /// Tokenized real-world asset (Tanastok DCNFT, Careaway plan, …).
    /// `id_bytes` is typically `keccak256("dcnft://<contract>/<token>")`.
    Asset,
    /// ONCHAINID / Datawallet+ identity. `id_bytes` is the ONCHAINID address.
    Did,
    /// The single global federation cord — anchor knots are appended here
    /// every ~3s. Exactly ONE cord exists per network.
    Cord,
}

impl StringKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            StringKind::Wallet => "wallet",
            StringKind::Contract => "contract",
            StringKind::Asset => "asset",
            StringKind::Did => "did",
            StringKind::Cord => "cord",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "wallet" => Some(StringKind::Wallet),
            "contract" => Some(StringKind::Contract),
            "asset" => Some(StringKind::Asset),
            "did" => Some(StringKind::Did),
            "cord" => Some(StringKind::Cord),
            _ => None,
        }
    }
}

/// Metadata describing a string's current state — Quipu Canon v1.2.
///
/// Stored in the [`StringRegistry`] and does NOT contain the actual knot
/// payloads. Field names preserve the v1.0/1.1 schema (`wallet_address`,
/// `genesis_string_id`, `head_string_id`, `entry_count`) for wire
/// compatibility — see helper accessors below for the canonical Quipu
/// Canon v1.2 names (`id_bytes`, `genesis_knot_id`, `head_knot_id`,
/// `knot_count`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerDescriptor {
    /// Kind of entity this string represents. Defaults to `Wallet` when
    /// deserialising legacy v1.0/1.1 descriptors.
    #[serde(default)]
    pub kind: StringKind,
    /// Raw byte-string identifying the entity within its kind.
    /// For wallets/contracts: the 20-byte EVM address.
    /// For assets/DIDs: an arbitrary identifier blob.
    /// For the cord: 32 zero bytes.
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
    /// Quipu Canon v1.2 accessor: the raw entity-id bytes.
    pub fn id_bytes(&self) -> &[u8] {
        &self.wallet_address
    }

    /// Hex rendering of the entity id, prefixed with `0x`. Stable across
    /// kinds (wallet/contract/asset/did/cord all share the same encoding).
    pub fn string_id_hex(&self) -> String {
        format!("0x{}", hex::encode(&self.wallet_address))
    }

    /// Backward-compat alias for `string_id_hex()`.
    pub fn wallet_hex(&self) -> String {
        self.string_id_hex()
    }

    pub fn string_id_kind(&self) -> StringKind {
        self.kind
    }

    /// Quipu Canon v1.2 name for `genesis_string_id` (which is actually
    /// a knot ID — see quipu-canon-v1.2-string-registry.mdc).
    pub fn genesis_knot_id(&self) -> StringId {
        self.genesis_string_id
    }

    /// Quipu Canon v1.2 name for `head_string_id`.
    pub fn head_knot_id(&self) -> StringId {
        self.head_string_id
    }

    /// Quipu Canon v1.2 name for `entry_count`.
    pub fn knot_count(&self) -> u64 {
        self.entry_count
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

/// Registry of strings indexed by `(StringKind, id_bytes)`.
///
/// Quipu Canon v1.2: every ecosystem entity that records knots — a
/// wallet, a smart contract, a tokenized asset, a DID, or the global
/// federation cord — owns exactly one string here. The previous name
/// `LedgerRegistry` is preserved as a deprecated type alias.
///
/// Thread-safe for concurrent node operation.
pub struct StringRegistry {
    /// Composite key = (kind, id_bytes). Keeps two distinct entities
    /// that share the same byte-id (e.g. wallet 0xABC vs contract 0xABC)
    /// from colliding.
    ledgers: RwLock<HashMap<(StringKind, Vec<u8>), LedgerDescriptor>>,
    genesis_index: RwLock<HashMap<(StringKind, Vec<u8>), StringId>>,
    head_index: RwLock<HashMap<(StringKind, Vec<u8>), StringId>>,
    /// Reverse lookup: which (kind, id) does a given knot belong to?
    knot_to_owner: RwLock<HashMap<StringId, (StringKind, Vec<u8>)>>,
}

impl StringRegistry {
    pub fn new() -> Self {
        Self {
            ledgers: RwLock::new(HashMap::new()),
            genesis_index: RwLock::new(HashMap::new()),
            head_index: RwLock::new(HashMap::new()),
            knot_to_owner: RwLock::new(HashMap::new()),
        }
    }

    // ---------------------------------------------------------------
    // Quipu Canon v1.2 — generic string API
    // ---------------------------------------------------------------

    /// Register a new string for any ecosystem entity.
    pub fn create_string(
        &self,
        kind: StringKind,
        id_bytes: &[u8],
        genesis_id: StringId,
        oes_generation: u64,
        replication_factor: u32,
    ) -> Result<LedgerDescriptor, LedgerRegistryError> {
        let key = (kind, id_bytes.to_vec());
        let mut ledgers = self.ledgers.write();
        if ledgers.contains_key(&key) {
            return Err(LedgerRegistryError::AlreadyExists(format!(
                "{}:{}",
                kind.as_str(),
                hex::encode(id_bytes)
            )));
        }

        let now = chrono::Utc::now().timestamp();
        let descriptor = LedgerDescriptor {
            kind,
            wallet_address: id_bytes.to_vec(),
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

        ledgers.insert(key.clone(), descriptor.clone());
        self.genesis_index.write().insert(key.clone(), genesis_id);
        self.head_index.write().insert(key.clone(), genesis_id);
        self.knot_to_owner.write().insert(genesis_id, key);

        Ok(descriptor)
    }

    /// Record a new knot appended to a string.
    pub fn record_knot(
        &self,
        kind: StringKind,
        id_bytes: &[u8],
        new_knot_id: StringId,
        content_size: u64,
        oes_generation: u64,
    ) -> Result<(), LedgerRegistryError> {
        let key = (kind, id_bytes.to_vec());
        let mut ledgers = self.ledgers.write();
        let desc = ledgers.get_mut(&key).ok_or_else(|| {
            LedgerRegistryError::NotFound(format!("{}:{}", kind.as_str(), hex::encode(id_bytes)))
        })?;

        if desc.is_deleted {
            return Err(LedgerRegistryError::Deleted(format!(
                "{}:{}",
                kind.as_str(),
                hex::encode(id_bytes)
            )));
        }

        desc.head_string_id = new_knot_id;
        desc.entry_count += 1;
        desc.total_size_bytes += content_size;
        desc.current_oes_generation = oes_generation;
        desc.last_appended_at = chrono::Utc::now().timestamp();

        self.head_index.write().insert(key.clone(), new_knot_id);
        self.knot_to_owner.write().insert(new_knot_id, key);

        Ok(())
    }

    /// Look up the descriptor for any (kind, id).
    pub fn get_string(&self, kind: StringKind, id_bytes: &[u8]) -> Option<LedgerDescriptor> {
        self.ledgers.read().get(&(kind, id_bytes.to_vec())).cloned()
    }

    /// Mark a string as deleted (OES key destroyed). Generic over kind.
    pub fn mark_string_deleted(
        &self,
        kind: StringKind,
        id_bytes: &[u8],
    ) -> Result<(), LedgerRegistryError> {
        let key = (kind, id_bytes.to_vec());
        let mut ledgers = self.ledgers.write();
        let desc = ledgers.get_mut(&key).ok_or_else(|| {
            LedgerRegistryError::NotFound(format!("{}:{}", kind.as_str(), hex::encode(id_bytes)))
        })?;
        desc.is_deleted = true;
        desc.deleted_at = Some(chrono::Utc::now().timestamp());
        Ok(())
    }

    /// All descriptors of a given kind.
    pub fn descriptors_by_kind(&self, kind: StringKind) -> Vec<LedgerDescriptor> {
        self.ledgers
            .read()
            .iter()
            .filter(|((k, _), _)| *k == kind)
            .map(|(_, d)| d.clone())
            .collect()
    }

    /// Quipu Canon v1.2 — total registered strings (across all kinds).
    pub fn strings_count(&self) -> usize {
        self.ledgers.read().len()
    }

    /// Quipu Canon v1.2 — total knots, summed across all strings.
    /// Invariant: `knots_count() >= strings_count()` (each string has at
    /// least its genesis knot).
    pub fn knots_count(&self) -> u64 {
        self.ledgers.read().values().map(|d| d.entry_count).sum()
    }

    /// Per-kind counts. Useful for the `rope_globalStats` RPC method.
    pub fn counts_by_kind(&self) -> HashMap<StringKind, (usize, u64)> {
        let mut out = HashMap::new();
        for ((k, _), d) in self.ledgers.read().iter() {
            let entry = out.entry(*k).or_insert((0usize, 0u64));
            entry.0 += 1;
            entry.1 += d.entry_count;
        }
        out
    }

    /// Look up which (kind, id) owns a knot.
    pub fn owner_of_knot(&self, knot_id: &StringId) -> Option<(StringKind, Vec<u8>)> {
        self.knot_to_owner.read().get(knot_id).cloned()
    }

    // ---------------------------------------------------------------
    // Backward-compat wallet-only API (delegates to the generic methods
    // above with `kind = Wallet`). All existing callers keep working
    // unchanged.
    // ---------------------------------------------------------------

    /// Backward-compat: register a new personal ledger for a wallet.
    pub fn create_ledger(
        &self,
        wallet_address: &[u8],
        genesis_id: StringId,
        oes_generation: u64,
        replication_factor: u32,
    ) -> Result<LedgerDescriptor, LedgerRegistryError> {
        self.create_string(
            StringKind::Wallet,
            wallet_address,
            genesis_id,
            oes_generation,
            replication_factor,
        )
    }

    /// Backward-compat alias for [`record_knot`] keyed to the Wallet kind.
    pub fn record_append(
        &self,
        wallet_address: &[u8],
        new_string_id: StringId,
        content_size: u64,
        oes_generation: u64,
    ) -> Result<(), LedgerRegistryError> {
        self.record_knot(
            StringKind::Wallet,
            wallet_address,
            new_string_id,
            content_size,
            oes_generation,
        )
    }

    /// Backward-compat: mark a wallet's ledger as deleted.
    pub fn mark_deleted(&self, wallet_address: &[u8]) -> Result<(), LedgerRegistryError> {
        self.mark_string_deleted(StringKind::Wallet, wallet_address)
    }

    /// Backward-compat: look up the descriptor for a wallet.
    pub fn get_descriptor(&self, wallet_address: &[u8]) -> Option<LedgerDescriptor> {
        self.get_string(StringKind::Wallet, wallet_address)
    }

    /// Backward-compat: look up which wallet owns a given knot.
    /// Returns `None` if the owning string is not of `Wallet` kind.
    pub fn wallet_for_string(&self, string_id: &StringId) -> Option<Vec<u8>> {
        self.knot_to_owner.read().get(string_id).and_then(|(k, b)| {
            if *k == StringKind::Wallet {
                Some(b.clone())
            } else {
                None
            }
        })
    }

    pub fn head_for_wallet(&self, wallet_address: &[u8]) -> Option<StringId> {
        self.head_index
            .read()
            .get(&(StringKind::Wallet, wallet_address.to_vec()))
            .copied()
    }

    pub fn genesis_for_wallet(&self, wallet_address: &[u8]) -> Option<StringId> {
        self.genesis_index
            .read()
            .get(&(StringKind::Wallet, wallet_address.to_vec()))
            .copied()
    }

    pub fn all_wallets(&self) -> Vec<Vec<u8>> {
        self.ledgers
            .read()
            .iter()
            .filter(|((k, _), _)| *k == StringKind::Wallet)
            .map(|((_, b), _)| b.clone())
            .collect()
    }

    /// Count of active (non-deleted) wallet ledgers.
    pub fn active_count(&self) -> usize {
        self.ledgers
            .read()
            .iter()
            .filter(|((k, _), d)| *k == StringKind::Wallet && !d.is_deleted)
            .count()
    }

    /// Count of all wallet ledgers.
    pub fn total_count(&self) -> usize {
        self.ledgers
            .read()
            .iter()
            .filter(|((k, _), _)| *k == StringKind::Wallet)
            .count()
    }
}

impl Default for StringRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Deprecated alias retained for v1.0/1.1 callers. Use [`StringRegistry`].
#[deprecated(
    since = "0.2.0",
    note = "Use `StringRegistry` (Quipu Canon v1.2). The old name conflated \
            the per-entity logical chain (string) with its individual \
            event entries (knots)."
)]
pub type LedgerRegistry = StringRegistry;

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
#[allow(deprecated)]
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
    fn test_string_registry_invariant_holds_with_mixed_kinds() {
        // Quipu Canon v1.2 — count(strings) <= count(knots)
        let registry = StringRegistry::new();

        // Two wallet strings, each with one extra knot beyond genesis.
        let wallet_a = vec![0xA1; 20];
        let wallet_b = vec![0xB2; 20];
        let g_a = StringId::from_content(b"wa-genesis");
        let g_b = StringId::from_content(b"wb-genesis");
        registry
            .create_string(StringKind::Wallet, &wallet_a, g_a, 0, 1)
            .unwrap();
        registry
            .create_string(StringKind::Wallet, &wallet_b, g_b, 0, 1)
            .unwrap();
        registry
            .record_knot(
                StringKind::Wallet,
                &wallet_a,
                StringId::from_content(b"wa-1"),
                32,
                0,
            )
            .unwrap();

        // One contract string with just its genesis.
        let contract = vec![0xC3; 20];
        let g_c = StringId::from_content(b"contract-genesis");
        registry
            .create_string(StringKind::Contract, &contract, g_c, 0, 1)
            .unwrap();

        // One asset string keyed by a 32-byte derived id.
        let asset_id = vec![0xDD; 32];
        let g_d = StringId::from_content(b"asset-genesis");
        registry
            .create_string(StringKind::Asset, &asset_id, g_d, 0, 1)
            .unwrap();

        // 4 strings; knots = 2 wallet genesis + 1 wallet append + 1 contract
        // genesis + 1 asset genesis = 5.
        assert_eq!(registry.strings_count(), 4);
        assert_eq!(registry.knots_count(), 5);
        assert!(registry.knots_count() >= registry.strings_count() as u64);

        // Wallet 0xA1 and contract 0xA1 must NOT collide (different kinds).
        let collide = vec![0xA1; 20];
        registry
            .create_string(
                StringKind::Contract,
                &collide,
                StringId::from_content(b"c-a1"),
                0,
                1,
            )
            .unwrap();
        assert_eq!(registry.strings_count(), 5);
    }

    #[test]
    fn test_string_kind_parse_roundtrip() {
        for k in [
            StringKind::Wallet,
            StringKind::Contract,
            StringKind::Asset,
            StringKind::Did,
            StringKind::Cord,
        ] {
            assert_eq!(StringKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(StringKind::parse("WALLET"), Some(StringKind::Wallet));
        assert_eq!(StringKind::parse("nope"), None);
    }

    #[test]
    fn test_descriptor_v12_accessors() {
        let registry = StringRegistry::new();
        let wallet = test_wallet();
        let genesis = StringId::from_content(b"genesis");
        let desc = registry.create_ledger(&wallet, genesis, 0, 5).unwrap();
        assert_eq!(desc.string_id_kind(), StringKind::Wallet);
        assert_eq!(desc.id_bytes(), wallet.as_slice());
        assert_eq!(desc.genesis_knot_id(), genesis);
        assert_eq!(desc.head_knot_id(), genesis);
        assert_eq!(desc.knot_count(), 1);
        assert!(desc.string_id_hex().starts_with("0x"));
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
            kind: StringKind::Wallet,
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
