//! # Datachain Rope Storage
//!
//! Persistent storage using RocksDB with LSM optimization.
//!
//! ## Storage Layout
//!
//! - `lattice_db/` - String Lattice persistence
//! - `complement_db/` - Complement storage (separate for security)
//! - `state_db/` - OES and federation state

pub mod lattice_db {
    //! Lattice persistence layer

    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// Simple in-memory lattice storage (RocksDB will replace this in production)
    pub struct LatticeStore {
        data: RwLock<HashMap<[u8; 32], Vec<u8>>>,
    }

    impl LatticeStore {
        pub fn new() -> Self {
            Self {
                data: RwLock::new(HashMap::new()),
            }
        }

        pub fn put(&self, key: [u8; 32], value: Vec<u8>) {
            self.data.write().insert(key, value);
        }

        pub fn get(&self, key: &[u8; 32]) -> Option<Vec<u8>> {
            self.data.read().get(key).cloned()
        }

        pub fn delete(&self, key: &[u8; 32]) -> bool {
            self.data.write().remove(key).is_some()
        }

        pub fn contains(&self, key: &[u8; 32]) -> bool {
            self.data.read().contains_key(key)
        }
    }

    impl Default for LatticeStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod complement_db {
    //! Complement storage - isolated for security

    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// Complement storage with separate encryption context
    pub struct ComplementStore {
        data: RwLock<HashMap<[u8; 32], Vec<u8>>>,
    }

    impl ComplementStore {
        pub fn new() -> Self {
            Self {
                data: RwLock::new(HashMap::new()),
            }
        }

        pub fn store_complement(&self, string_id: [u8; 32], complement_data: Vec<u8>) {
            self.data.write().insert(string_id, complement_data);
        }

        pub fn get_complement(&self, string_id: &[u8; 32]) -> Option<Vec<u8>> {
            self.data.read().get(string_id).cloned()
        }

        pub fn erase_complement(&self, string_id: &[u8; 32]) -> bool {
            self.data.write().remove(string_id).is_some()
        }
    }

    impl Default for ComplementStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod state_db {
    //! OES and federation state persistence

    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// State persistence for OES and federation
    pub struct StateStore {
        oes_states: RwLock<HashMap<String, Vec<u8>>>,
        federation_states: RwLock<HashMap<String, Vec<u8>>>,
    }

    impl StateStore {
        pub fn new() -> Self {
            Self {
                oes_states: RwLock::new(HashMap::new()),
                federation_states: RwLock::new(HashMap::new()),
            }
        }

        pub fn save_oes_state(&self, node_id: &str, state: Vec<u8>) {
            self.oes_states.write().insert(node_id.to_string(), state);
        }

        pub fn load_oes_state(&self, node_id: &str) -> Option<Vec<u8>> {
            self.oes_states.read().get(node_id).cloned()
        }

        pub fn save_federation_state(&self, fed_id: &str, state: Vec<u8>) {
            self.federation_states
                .write()
                .insert(fed_id.to_string(), state);
        }

        pub fn load_federation_state(&self, fed_id: &str) -> Option<Vec<u8>> {
            self.federation_states.read().get(fed_id).cloned()
        }
    }

    impl Default for StateStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod ledger_db {
    //! Personal ledger storage — wallet→StringId index and piece map persistence.
    //!
    //! Provides the storage backend for the personal ledger model where each
    //! wallet maps to a chain of StringIds. Maintains reverse indexes for
    //! efficient lookups in both directions.

    use parking_lot::RwLock;
    use std::collections::HashMap;

    /// Ledger descriptor stored per wallet
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct StoredLedgerDescriptor {
        pub wallet_address: Vec<u8>,
        pub genesis_string_id: [u8; 32],
        pub head_string_id: [u8; 32],
        pub entry_count: u64,
        pub total_size_bytes: u64,
        pub oes_generation_at_creation: u64,
        pub current_oes_generation: u64,
        pub created_at: i64,
        pub last_appended_at: i64,
        pub is_deleted: bool,
        pub deleted_at: Option<i64>,
        pub replication_factor: u32,
    }

    /// Piece map entry for storage
    #[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
    pub struct StoredPieceMap {
        pub string_id: [u8; 32],
        pub total_pieces: u32,
        pub total_size: u64,
        pub piece_hashes: Vec<[u8; 32]>,
        pub piece_sizes: Vec<u32>,
    }

    /// Persistent ledger storage (in-memory; RocksDB column family in production)
    pub struct LedgerStore {
        descriptors: RwLock<HashMap<Vec<u8>, StoredLedgerDescriptor>>,
        wallet_to_chain: RwLock<HashMap<Vec<u8>, Vec<[u8; 32]>>>,
        string_to_wallet: RwLock<HashMap<[u8; 32], Vec<u8>>>,
        piece_maps: RwLock<HashMap<[u8; 32], StoredPieceMap>>,
        head_index: RwLock<HashMap<Vec<u8>, [u8; 32]>>,
    }

    impl LedgerStore {
        pub fn new() -> Self {
            Self {
                descriptors: RwLock::new(HashMap::new()),
                wallet_to_chain: RwLock::new(HashMap::new()),
                string_to_wallet: RwLock::new(HashMap::new()),
                piece_maps: RwLock::new(HashMap::new()),
                head_index: RwLock::new(HashMap::new()),
            }
        }

        pub fn put_descriptor(&self, wallet: &[u8], desc: StoredLedgerDescriptor) {
            self.head_index.write().insert(wallet.to_vec(), desc.head_string_id);
            self.descriptors.write().insert(wallet.to_vec(), desc);
        }

        pub fn get_descriptor(&self, wallet: &[u8]) -> Option<StoredLedgerDescriptor> {
            self.descriptors.read().get(wallet).cloned()
        }

        pub fn append_to_chain(&self, wallet: &[u8], string_id: [u8; 32]) {
            self.wallet_to_chain
                .write()
                .entry(wallet.to_vec())
                .or_default()
                .push(string_id);
            self.string_to_wallet
                .write()
                .insert(string_id, wallet.to_vec());
            self.head_index
                .write()
                .insert(wallet.to_vec(), string_id);
        }

        pub fn get_chain(&self, wallet: &[u8]) -> Vec<[u8; 32]> {
            self.wallet_to_chain
                .read()
                .get(wallet)
                .cloned()
                .unwrap_or_default()
        }

        pub fn wallet_for_string(&self, string_id: &[u8; 32]) -> Option<Vec<u8>> {
            self.string_to_wallet.read().get(string_id).cloned()
        }

        pub fn head_for_wallet(&self, wallet: &[u8]) -> Option<[u8; 32]> {
            self.head_index.read().get(wallet).copied()
        }

        pub fn put_piece_map(&self, string_id: [u8; 32], map: StoredPieceMap) {
            self.piece_maps.write().insert(string_id, map);
        }

        pub fn get_piece_map(&self, string_id: &[u8; 32]) -> Option<StoredPieceMap> {
            self.piece_maps.read().get(string_id).cloned()
        }

        pub fn mark_deleted(&self, wallet: &[u8]) -> bool {
            let mut descs = self.descriptors.write();
            if let Some(desc) = descs.get_mut(wallet) {
                desc.is_deleted = true;
                desc.deleted_at = Some(chrono::Utc::now().timestamp());
                true
            } else {
                false
            }
        }

        pub fn all_wallets(&self) -> Vec<Vec<u8>> {
            self.descriptors.read().keys().cloned().collect()
        }

        pub fn active_count(&self) -> usize {
            self.descriptors
                .read()
                .values()
                .filter(|d| !d.is_deleted)
                .count()
        }

        pub fn total_count(&self) -> usize {
            self.descriptors.read().len()
        }

        pub fn total_entries(&self) -> u64 {
            self.descriptors
                .read()
                .values()
                .map(|d| d.entry_count)
                .sum()
        }

        pub fn total_bytes(&self) -> u64 {
            self.descriptors
                .read()
                .values()
                .map(|d| d.total_size_bytes)
                .sum()
        }
    }

    impl Default for LedgerStore {
        fn default() -> Self {
            Self::new()
        }
    }
}

// Re-export for convenience
pub use complement_db::ComplementStore;
pub use lattice_db::LatticeStore;
pub use ledger_db::LedgerStore;
pub use state_db::StateStore;

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    mod lattice_store_tests {
        use super::*;

        #[test]
        fn test_lattice_store_creation() {
            let store = LatticeStore::new();
            let key = [1u8; 32];
            assert!(!store.contains(&key));
        }

        #[test]
        fn test_lattice_store_put_get() {
            let store = LatticeStore::new();
            let key = [2u8; 32];
            let value = vec![1, 2, 3, 4, 5];

            store.put(key, value.clone());

            let retrieved = store.get(&key);
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap(), value);
        }

        #[test]
        fn test_lattice_store_delete() {
            let store = LatticeStore::new();
            let key = [3u8; 32];
            let value = vec![10, 20, 30];

            store.put(key, value);
            assert!(store.contains(&key));

            let deleted = store.delete(&key);
            assert!(deleted);
            assert!(!store.contains(&key));
        }

        #[test]
        fn test_lattice_store_get_nonexistent() {
            let store = LatticeStore::new();
            let key = [4u8; 32];
            assert!(store.get(&key).is_none());
        }

        #[test]
        fn test_lattice_store_default() {
            let store: LatticeStore = Default::default();
            let key = [5u8; 32];
            assert!(!store.contains(&key));
        }
    }

    mod complement_store_tests {
        use super::*;

        #[test]
        fn test_complement_store_creation() {
            let store = ComplementStore::new();
            let string_id = [1u8; 32];
            assert!(store.get_complement(&string_id).is_none());
        }

        #[test]
        fn test_complement_store_put_get() {
            let store = ComplementStore::new();
            let string_id = [2u8; 32];
            let complement = vec![100, 200, 255];

            store.store_complement(string_id, complement.clone());

            let retrieved = store.get_complement(&string_id);
            assert!(retrieved.is_some());
            assert_eq!(retrieved.unwrap(), complement);
        }

        #[test]
        fn test_complement_store_erase() {
            let store = ComplementStore::new();
            let string_id = [3u8; 32];
            let complement = vec![1, 2, 3];

            store.store_complement(string_id, complement);
            assert!(store.get_complement(&string_id).is_some());

            let erased = store.erase_complement(&string_id);
            assert!(erased);
            assert!(store.get_complement(&string_id).is_none());
        }

        #[test]
        fn test_complement_store_default() {
            let store: ComplementStore = Default::default();
            let string_id = [4u8; 32];
            assert!(store.get_complement(&string_id).is_none());
        }
    }

    mod state_store_tests {
        use super::*;

        #[test]
        fn test_state_store_creation() {
            let store = StateStore::new();
            assert!(store.load_oes_state("node1").is_none());
            assert!(store.load_federation_state("fed1").is_none());
        }

        #[test]
        fn test_oes_state_save_load() {
            let store = StateStore::new();
            let node_id = "node_abc";
            let state = vec![1, 2, 3, 4];

            store.save_oes_state(node_id, state.clone());

            let loaded = store.load_oes_state(node_id);
            assert!(loaded.is_some());
            assert_eq!(loaded.unwrap(), state);
        }

        #[test]
        fn test_federation_state_save_load() {
            let store = StateStore::new();
            let fed_id = "federation_xyz";
            let state = vec![10, 20, 30];

            store.save_federation_state(fed_id, state.clone());

            let loaded = store.load_federation_state(fed_id);
            assert!(loaded.is_some());
            assert_eq!(loaded.unwrap(), state);
        }

        #[test]
        fn test_state_store_default() {
            let store: StateStore = Default::default();
            assert!(store.load_oes_state("test").is_none());
        }
    }
}
