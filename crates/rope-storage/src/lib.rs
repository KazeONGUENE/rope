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
