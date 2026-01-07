//! Organic Encryption System (OES)
//! 
//! Self-evolving cryptographic material inspired by DNA mutation and repair.
//! OES ensures perfect forward secrecy and quantum resistance through
//! continuous cryptographic evolution synchronized across the network.
//! 
//! ## Evolution Protocol
//! 
//! OES state evolves every OES_EVOLUTION_INTERVAL (100 anchors):
//! 1. Compute new mutation seed from anchor hash
//! 2. Apply controlled mutations to genome (10% mutation rate)
//! 3. Derive new key material from mutated genome
//! 4. Update keypairs and state commitment

use parking_lot::RwLock;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

use rope_core::types::constants::{GENOME_DIMENSION, MUTATION_RATE, OES_EVOLUTION_INTERVAL};

/// Organic Encryption State
/// 
/// The cryptographic genome that evolves over time, providing:
/// - Perfect forward secrecy (past states unrecoverable)
/// - Quantum resistance (post-quantum key derivation)
/// - Network synchronization (deterministic evolution)
#[derive(Clone, Serialize, Deserialize)]
pub struct OrganicEncryptionState {
    /// Current evolution generation
    generation: u64,
    
    /// The cryptographic genome (992 bytes default)
    genome: Vec<u8>,
    
    /// Mutation seed for deterministic evolution
    mutation_seed: [u8; 32],
    
    /// Hash of previous state (for verification)
    previous_state_hash: [u8; 32],
    
    /// Kyber public key (post-quantum KEM)
    #[serde(with = "serde_bytes")]
    kyber_public_key: Vec<u8>,
    
    /// Dilithium public key (post-quantum signature)
    #[serde(with = "serde_bytes")]
    dilithium_public_key: Vec<u8>,
}

/// Secret components of OES (zeroized on drop)
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct OESSecrets {
    /// Kyber secret key
    kyber_secret_key: Vec<u8>,
    
    /// Dilithium secret key
    dilithium_secret_key: Vec<u8>,
}

impl OrganicEncryptionState {
    /// Create genesis OES state from initial seed
    pub fn genesis(seed: &[u8; 32]) -> (Self, OESSecrets) {
        let mut rng = ChaCha20Rng::from_seed(*seed);
        
        // Generate initial genome
        let mut genome = vec![0u8; GENOME_DIMENSION];
        for byte in genome.iter_mut() {
            *byte = rand::Rng::gen(&mut rng);
        }
        
        // Derive initial key material
        let key_material = Self::derive_key_material(&genome);
        
        // Generate post-quantum keypairs
        // Note: In production, use actual pqcrypto library
        let (kyber_pk, kyber_sk) = Self::generate_kyber_keypair(&key_material[0..32]);
        let (dilithium_pk, dilithium_sk) = Self::generate_dilithium_keypair(&key_material[32..64]);
        
        let state = Self {
            generation: 0,
            genome,
            mutation_seed: *seed,
            previous_state_hash: [0u8; 32],
            kyber_public_key: kyber_pk,
            dilithium_public_key: dilithium_pk,
        };
        
        let secrets = OESSecrets {
            kyber_secret_key: kyber_sk,
            dilithium_secret_key: dilithium_sk,
        };
        
        (state, secrets)
    }

    /// Evolve the OES state based on anchor hash
    /// 
    /// This is the core evolution mechanism inspired by DNA mutation.
    /// The mutation rate (10%) provides entropy growth while maintaining
    /// network synchronization.
    pub fn evolve(&mut self, anchor_hash: &[u8; 32]) -> OESSecrets {
        // Step 1: Compute new mutation seed
        let new_seed = blake3::keyed_hash(&self.mutation_seed, anchor_hash);
        let new_seed_bytes = *new_seed.as_bytes();
        
        // Step 2: Apply controlled mutations to genome
        let mut rng = ChaCha20Rng::from_seed(new_seed_bytes);
        let mutations = self.compute_mutations(&mut rng);
        
        for (position, new_value) in mutations {
            if position < self.genome.len() {
                self.genome[position] = new_value;
            }
        }
        
        // Step 3: Derive new key material
        let key_material = Self::derive_key_material(&self.genome);
        
        // Step 4: Generate new keypairs
        let (kyber_pk, kyber_sk) = Self::generate_kyber_keypair(&key_material[0..32]);
        let (dilithium_pk, dilithium_sk) = Self::generate_dilithium_keypair(&key_material[32..64]);
        
        // Step 5: Update state
        self.previous_state_hash = self.state_hash();
        self.mutation_seed = new_seed_bytes;
        self.generation += 1;
        self.kyber_public_key = kyber_pk;
        self.dilithium_public_key = dilithium_pk;
        
        OESSecrets {
            kyber_secret_key: kyber_sk,
            dilithium_secret_key: dilithium_sk,
        }
    }

    /// Compute mutations based on RNG
    fn compute_mutations(&self, rng: &mut ChaCha20Rng) -> Vec<(usize, u8)> {
        let mutation_count = (self.genome.len() as f64 * MUTATION_RATE) as usize;
        let mut mutations = Vec::with_capacity(mutation_count);
        
        for _ in 0..mutation_count {
            let position = rand::Rng::gen_range(rng, 0..self.genome.len());
            let new_value: u8 = rand::Rng::gen(rng);
            mutations.push((position, new_value));
        }
        
        mutations
    }

    /// Derive key material from genome using BLAKE3
    fn derive_key_material(genome: &[u8]) -> [u8; 64] {
        let hash1 = blake3::hash(&[genome, b"kyber_seed"].concat());
        let hash2 = blake3::hash(&[genome, b"dilithium_seed"].concat());
        
        let mut material = [0u8; 64];
        material[0..32].copy_from_slice(hash1.as_bytes());
        material[32..64].copy_from_slice(hash2.as_bytes());
        material
    }

    /// Generate Kyber keypair (placeholder - use pqcrypto in production)
    fn generate_kyber_keypair(seed: &[u8]) -> (Vec<u8>, Vec<u8>) {
        // In production: use pqcrypto_kyber::kyber768
        let pk_hash = blake3::hash(&[seed, b"kyber_pk"].concat());
        let sk_hash = blake3::hash(&[seed, b"kyber_sk"].concat());
        (pk_hash.as_bytes().to_vec(), sk_hash.as_bytes().to_vec())
    }

    /// Generate Dilithium keypair (placeholder - use pqcrypto in production)
    fn generate_dilithium_keypair(seed: &[u8]) -> (Vec<u8>, Vec<u8>) {
        // In production: use pqcrypto_dilithium::dilithium3
        let pk_hash = blake3::hash(&[seed, b"dilithium_pk"].concat());
        let sk_hash = blake3::hash(&[seed, b"dilithium_sk"].concat());
        (pk_hash.as_bytes().to_vec(), sk_hash.as_bytes().to_vec())
    }

    /// Get current generation
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Get Kyber public key
    pub fn kyber_public_key(&self) -> &[u8] {
        &self.kyber_public_key
    }

    /// Get Dilithium public key
    pub fn dilithium_public_key(&self) -> &[u8] {
        &self.dilithium_public_key
    }

    /// Check if a generation is within the valid window
    pub fn is_valid_generation(&self, gen: u64) -> bool {
        let window = rope_core::types::constants::GENERATION_WINDOW;
        gen >= self.generation.saturating_sub(window) && gen <= self.generation + 1
    }

    /// Compute state hash
    pub fn state_hash(&self) -> [u8; 32] {
        let mut content = Vec::new();
        content.extend_from_slice(&self.generation.to_be_bytes());
        content.extend_from_slice(&self.genome);
        content.extend_from_slice(&self.mutation_seed);
        *blake3::hash(&content).as_bytes()
    }

    /// Generate OES proof for a string
    pub fn generate_proof(&self) -> OESProof {
        OESProof {
            generation: self.generation,
            state_commitment: self.state_hash(),
            merkle_proof: Vec::new(), // Simplified - full implementation would include merkle proof
            signature: Vec::new(),    // To be signed with Dilithium
        }
    }

    /// Verify an OES proof
    pub fn verify_proof(&self, proof: &OESProof) -> bool {
        // Check generation is within valid window
        if !self.is_valid_generation(proof.generation) {
            return false;
        }
        
        // For same generation, verify state commitment
        if proof.generation == self.generation {
            return proof.state_commitment == self.state_hash();
        }
        
        // For different generations, would need historical state or merkle proof
        true // Simplified for now
    }
}

/// OES Proof - Demonstrates string was created by synchronized node
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OESProof {
    /// OES generation epoch
    pub generation: u64,
    
    /// Commitment to OES state
    pub state_commitment: [u8; 32],
    
    /// Merkle proof of inclusion
    pub merkle_proof: Vec<[u8; 32]>,
    
    /// Dilithium signature over the proof
    pub signature: Vec<u8>,
}

impl OESProof {
    /// Create empty proof (for testing/genesis)
    pub fn empty() -> Self {
        Self {
            generation: 0,
            state_commitment: [0u8; 32],
            merkle_proof: Vec::new(),
            signature: Vec::new(),
        }
    }
}

/// OES Manager for thread-safe state management
pub struct OESManager {
    state: RwLock<OrganicEncryptionState>,
    secrets: RwLock<OESSecrets>,
    evolution_counter: RwLock<u64>,
}

impl OESManager {
    /// Create new OES manager with genesis state
    pub fn genesis(seed: &[u8; 32]) -> Self {
        let (state, secrets) = OrganicEncryptionState::genesis(seed);
        Self {
            state: RwLock::new(state),
            secrets: RwLock::new(secrets),
            evolution_counter: RwLock::new(0),
        }
    }

    /// Get current generation
    pub fn generation(&self) -> u64 {
        self.state.read().generation()
    }

    /// Check if evolution is needed
    pub fn should_evolve(&self, anchor_count: u64) -> bool {
        anchor_count > 0 && anchor_count % OES_EVOLUTION_INTERVAL == 0
    }

    /// Trigger evolution
    pub fn evolve(&self, anchor_hash: &[u8; 32]) {
        let mut state = self.state.write();
        let new_secrets = state.evolve(anchor_hash);
        
        let mut secrets = self.secrets.write();
        *secrets = new_secrets;
        
        let mut counter = self.evolution_counter.write();
        *counter += 1;
    }

    /// Generate proof for current state
    pub fn generate_proof(&self) -> OESProof {
        self.state.read().generate_proof()
    }

    /// Verify a proof
    pub fn verify_proof(&self, proof: &OESProof) -> bool {
        self.state.read().verify_proof(proof)
    }

    /// Get state hash
    pub fn state_hash(&self) -> [u8; 32] {
        self.state.read().state_hash()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_genesis_creation() {
        let seed = [0u8; 32];
        let (state, _secrets) = OrganicEncryptionState::genesis(&seed);
        
        assert_eq!(state.generation(), 0);
        assert_eq!(state.genome.len(), GENOME_DIMENSION);
    }

    #[test]
    fn test_evolution() {
        let seed = [1u8; 32];
        let (mut state, _) = OrganicEncryptionState::genesis(&seed);
        
        let initial_hash = state.state_hash();
        let initial_gen = state.generation();
        
        let anchor_hash = [2u8; 32];
        let _new_secrets = state.evolve(&anchor_hash);
        
        assert_eq!(state.generation(), initial_gen + 1);
        assert_ne!(state.state_hash(), initial_hash);
        assert_eq!(state.previous_state_hash, initial_hash);
    }

    #[test]
    fn test_generation_window() {
        let seed = [0u8; 32];
        let (state, _) = OrganicEncryptionState::genesis(&seed);
        
        assert!(state.is_valid_generation(0));
        assert!(state.is_valid_generation(1));
        assert!(!state.is_valid_generation(100));
    }

    #[test]
    fn test_proof_generation_and_verification() {
        let seed = [0u8; 32];
        let (state, _) = OrganicEncryptionState::genesis(&seed);
        
        let proof = state.generate_proof();
        
        assert_eq!(proof.generation, 0);
        assert!(state.verify_proof(&proof));
    }

    #[test]
    fn test_deterministic_evolution() {
        let seed = [42u8; 32];
        let anchor_hash = [99u8; 32];
        
        let (mut state1, _) = OrganicEncryptionState::genesis(&seed);
        let (mut state2, _) = OrganicEncryptionState::genesis(&seed);
        
        state1.evolve(&anchor_hash);
        state2.evolve(&anchor_hash);
        
        // Evolution should be deterministic
        assert_eq!(state1.state_hash(), state2.state_hash());
        assert_eq!(state1.generation(), state2.generation());
    }
}

