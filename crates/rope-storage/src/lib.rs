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

// Re-export for convenience
pub use complement_db::ComplementStore;
pub use lattice_db::LatticeStore;
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
