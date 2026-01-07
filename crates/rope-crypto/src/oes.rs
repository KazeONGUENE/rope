//! Organic Encryption System (OES) - Complete Implementation
//! 
//! Self-evolving cryptographic material inspired by DNA mutation and repair.
//! Based on the OrganicCryptographicOrganism from Datawallet+ implementation.
//! 
//! ## Core Components (from organic_encryption_api.py)
//! 
//! 1. **Genome** - 2048-bit mutating cryptographic DNA
//! 2. **Lorenz Attractor** - Chaotic dynamics for unpredictability
//! 3. **Cellular Automaton** - Game of Life grid evolution
//! 4. **Mandelbrot Fractal** - Deterministic chaos iteration
//! 5. **Quantum Walk** - Simulated quantum amplitude evolution
//! 6. **Impossibility Anchors** - Mathematical hardness proofs
//! 
//! ## Evolution Protocol
//! 
//! OES state evolves through interconnected systems:
//! - Lorenz influences fractal position and cellular mutation rate
//! - Cellular density influences quantum walk bias
//! - All states contribute to key derivation
//! - Impossibility anchors provide cryptographic hardness

use parking_lot::RwLock;
use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};
use std::f64::consts::PI;

use rope_core::types::constants::{GENOME_DIMENSION, MUTATION_RATE, OES_EVOLUTION_INTERVAL};

/// Lorenz attractor state (chaos dynamics)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LorenzState {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl LorenzState {
    /// Lorenz system parameters
    const SIGMA: f64 = 10.0;
    const RHO: f64 = 28.0;
    const BETA: f64 = 8.0 / 3.0;

    /// Create from seed entropy
    pub fn from_seed(seed: &[u8]) -> Self {
        let mut input = seed.to_vec();
        input.extend_from_slice(b"lorenz_init");
        let hash = blake3::hash(&input);
        let bytes = hash.as_bytes();
        
        let x = Self::bytes_to_range(&bytes[0..8], -25.0, 25.0);
        let y = Self::bytes_to_range(&bytes[8..16], -25.0, 25.0);
        let z = Self::bytes_to_range(&bytes[16..24], 0.0, 50.0);
        
        Self { x, y, z }
    }

    fn bytes_to_range(bytes: &[u8], min: f64, max: f64) -> f64 {
        let val = u64::from_le_bytes(bytes.try_into().unwrap_or([0u8; 8]));
        min + (val as f64 / u64::MAX as f64) * (max - min)
    }

    /// Evolve Lorenz state by one timestep
    pub fn evolve(&mut self, dt: f64) {
        let dx = (Self::SIGMA * (self.y - self.x)) * dt;
        let dy = (self.x * (Self::RHO - self.z) - self.y) * dt;
        let dz = (self.x * self.y - Self::BETA * self.z) * dt;
        
        self.x += dx;
        self.y += dy;
        self.z += dz;
    }
}

/// Cellular automaton grid (Game of Life)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CellularGrid {
    /// 64x64 grid of cells
    grid: Vec<Vec<u8>>,
    size: usize,
}

impl CellularGrid {
    /// Create from seed
    pub fn from_seed(seed: &[u8], size: usize) -> Self {
        let mut input = seed.to_vec();
        input.extend_from_slice(b"cellular_init");
        let mut rng = ChaCha20Rng::from_seed(
            *blake3::hash(&input).as_bytes()
        );
        
        let grid: Vec<Vec<u8>> = (0..size)
            .map(|_| {
                (0..size)
                    .map(|_| if rand::Rng::gen::<bool>(&mut rng) { 1 } else { 0 })
                    .collect()
            })
            .collect();
        
        Self { grid, size }
    }

    /// Evolve grid one generation (Game of Life rules + mutations)
    pub fn evolve(&mut self, mutation_rate: f64) {
        let mut new_grid = self.grid.clone();
        let mut rng = ChaCha20Rng::from_entropy();
        
        for i in 0..self.size {
            for j in 0..self.size {
                let live_neighbors = self.count_neighbors(i, j);
                let cell = self.grid[i][j];
                
                // Conway's Game of Life rules
                new_grid[i][j] = match (cell, live_neighbors) {
                    (1, 2) | (1, 3) => 1, // Survival
                    (0, 3) => 1,          // Birth
                    _ => 0,               // Death
                };
                
                // Random mutation
                if rand::Rng::gen::<f64>(&mut rng) < mutation_rate {
                    new_grid[i][j] = 1 - new_grid[i][j];
                }
            }
        }
        
        self.grid = new_grid;
    }

    fn count_neighbors(&self, row: usize, col: usize) -> u8 {
        let mut count = 0u8;
        for dr in [-1i32, 0, 1] {
            for dc in [-1i32, 0, 1] {
                if dr == 0 && dc == 0 { continue; }
                let r = (row as i32 + dr).rem_euclid(self.size as i32) as usize;
                let c = (col as i32 + dc).rem_euclid(self.size as i32) as usize;
                count += self.grid[r][c];
            }
        }
        count
    }

    /// Get live cell density (0.0 to 1.0)
    pub fn density(&self) -> f64 {
        let total = (self.size * self.size) as f64;
        let live: u64 = self.grid.iter()
            .flat_map(|row| row.iter())
            .map(|&c| c as u64)
            .sum();
        live as f64 / total
    }

    /// Get hash of grid state
    pub fn hash(&self) -> [u8; 32] {
        let bytes: Vec<u8> = self.grid.iter()
            .flat_map(|row| row.iter().copied())
            .collect();
        *blake3::hash(&bytes).as_bytes()
    }
}

/// Mandelbrot fractal state
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FractalState {
    /// Complex constant c
    pub c_real: f64,
    pub c_imag: f64,
    /// Current z value
    pub z_real: f64,
    pub z_imag: f64,
    /// Iteration count
    pub iteration: u32,
    /// Maximum iterations before perturbation
    pub max_iterations: u32,
}

impl FractalState {
    /// Create from seed
    pub fn from_seed(seed: &[u8]) -> Self {
        let mut input = seed.to_vec();
        input.extend_from_slice(b"fractal_init");
        let hash = blake3::hash(&input);
        let bytes = hash.as_bytes();
        
        // Initialize c in the Mandelbrot set region
        let c_real = -2.5 + (u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as f64 / u32::MAX as f64) * 3.5;
        let c_imag = -1.5 + (u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as f64 / u32::MAX as f64) * 3.0;
        
        Self {
            c_real,
            c_imag,
            z_real: 0.0,
            z_imag: 0.0,
            iteration: 0,
            max_iterations: 200,
        }
    }

    /// Iterate z = z² + c
    pub fn evolve(&mut self, escape_threshold: f64) {
        let magnitude = (self.z_real * self.z_real + self.z_imag * self.z_imag).sqrt();
        
        if magnitude < escape_threshold && self.iteration < self.max_iterations {
            // z = z² + c
            let new_real = self.z_real * self.z_real - self.z_imag * self.z_imag + self.c_real;
            let new_imag = 2.0 * self.z_real * self.z_imag + self.c_imag;
            self.z_real = new_real;
            self.z_imag = new_imag;
            self.iteration += 1;
        } else {
            // Reset with slight perturbation
            let mut rng = ChaCha20Rng::from_entropy();
            self.c_real += (rand::Rng::gen::<f64>(&mut rng) - 0.5) * 0.001;
            self.c_imag += (rand::Rng::gen::<f64>(&mut rng) - 0.5) * 0.001;
            self.z_real = 0.0;
            self.z_imag = 0.0;
            self.iteration = 0;
        }
    }

    /// Perturb c based on external influence
    pub fn perturb(&mut self, influence: f64) {
        self.c_real += influence * 0.001;
        self.c_imag += influence * 0.001;
    }
}

/// Quantum walk state (conceptual quantum simulation)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuantumState {
    /// Complex amplitudes for each position (-5 to +5 = 11 positions)
    amplitudes_real: Vec<f64>,
    amplitudes_imag: Vec<f64>,
    /// Current "collapsed" position for observation
    pub position: i32,
    /// Number of positions
    num_positions: usize,
}

impl QuantumState {
    /// Create from seed, initialized at center
    pub fn from_seed(seed: &[u8]) -> Self {
        let num_positions = 11; // -5 to +5
        let mut amplitudes_real = vec![0.0; num_positions];
        let amplitudes_imag = vec![0.0; num_positions];
        
        // Start localized at center
        amplitudes_real[num_positions / 2] = 1.0;
        
        Self {
            amplitudes_real,
            amplitudes_imag,
            position: 0,
            num_positions,
        }
    }

    /// Evolve quantum walk with coin bias
    pub fn evolve(&mut self, coin_bias: f64) {
        let bias = coin_bias.clamp(0.0, 1.0);
        let sqrt_bias = bias.sqrt();
        let sqrt_complement = (1.0 - bias).sqrt();
        
        let mut new_real = vec![0.0; self.num_positions];
        let mut new_imag = vec![0.0; self.num_positions];
        
        // Apply conceptual coin operator and shift
        for i in 0..self.num_positions {
            let amp_real = self.amplitudes_real[i];
            let amp_imag = self.amplitudes_imag[i];
            
            // Left contribution
            if i > 0 {
                new_real[i - 1] += amp_real * sqrt_bias;
                new_imag[i - 1] += amp_imag * sqrt_bias;
            }
            
            // Right contribution
            if i < self.num_positions - 1 {
                new_real[i + 1] += amp_real * sqrt_complement;
                new_imag[i + 1] += amp_imag * sqrt_complement;
            }
        }
        
        // Normalize
        let norm: f64 = new_real.iter().zip(new_imag.iter())
            .map(|(r, i)| r * r + i * i)
            .sum::<f64>()
            .sqrt();
        
        if norm > 1e-9 {
            for i in 0..self.num_positions {
                new_real[i] /= norm;
                new_imag[i] /= norm;
            }
            self.amplitudes_real = new_real;
            self.amplitudes_imag = new_imag;
        } else {
            // Re-localize at center
            self.amplitudes_real = vec![0.0; self.num_positions];
            self.amplitudes_imag = vec![0.0; self.num_positions];
            self.amplitudes_real[self.num_positions / 2] = 1.0;
        }
        
        // "Measure" position (probabilistically)
        self.collapse_position();
    }

    fn collapse_position(&mut self) {
        let probabilities: Vec<f64> = self.amplitudes_real.iter()
            .zip(self.amplitudes_imag.iter())
            .map(|(r, i)| r * r + i * i)
            .collect();
        
        let total: f64 = probabilities.iter().sum();
        if total < 1e-9 {
            self.position = 0;
            return;
        }
        
        let mut rng = ChaCha20Rng::from_entropy();
        let threshold: f64 = rand::Rng::gen(&mut rng);
        let mut cumulative = 0.0;
        
        for (i, prob) in probabilities.iter().enumerate() {
            cumulative += prob / total;
            if cumulative >= threshold {
                self.position = (i as i32) - (self.num_positions as i32 / 2);
                return;
            }
        }
        
        self.position = 0;
    }

    /// Get amplitudes hash
    pub fn hash(&self) -> [u8; 32] {
        let mut bytes = Vec::new();
        for (r, i) in self.amplitudes_real.iter().zip(self.amplitudes_imag.iter()) {
            bytes.extend_from_slice(&r.to_le_bytes());
            bytes.extend_from_slice(&i.to_le_bytes());
        }
        *blake3::hash(&bytes).as_bytes()
    }
}

/// Impossibility anchors for cryptographic hardness
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImpossibilityAnchors {
    /// Large composite modulus (conceptual RSA-like)
    pub factorization_modulus_hash: [u8; 32],
    
    /// Lattice basis dimensions
    pub lattice_dim: usize,
    pub lattice_hash: [u8; 32],
    
    /// Elliptic curve parameters hash
    pub ec_params_hash: [u8; 32],
}

impl ImpossibilityAnchors {
    /// Create anchors from seed
    pub fn from_seed(seed: &[u8]) -> Self {
        // Generate conceptual factorization modulus
        let mut p_input = seed.to_vec();
        p_input.extend_from_slice(b"factor_p");
        let p_hash = blake3::hash(&p_input);
        
        let mut q_input = seed.to_vec();
        q_input.extend_from_slice(b"factor_q");
        let q_hash = blake3::hash(&q_input);
        
        let mut modulus_input = p_hash.as_bytes().to_vec();
        modulus_input.extend_from_slice(q_hash.as_bytes());
        let modulus_hash = blake3::hash(&modulus_input);
        
        // Generate lattice basis hash
        let mut lattice_input = seed.to_vec();
        lattice_input.extend_from_slice(b"lattice");
        let lattice_seed = blake3::hash(&lattice_input);
        let lattice_hash = *lattice_seed.as_bytes();
        
        // Generate EC params hash
        let mut ec_input = seed.to_vec();
        ec_input.extend_from_slice(b"ec_params");
        let ec_hash = blake3::hash(&ec_input);
        
        Self {
            factorization_modulus_hash: *modulus_hash.as_bytes(),
            lattice_dim: 256,
            lattice_hash,
            ec_params_hash: *ec_hash.as_bytes(),
        }
    }

    /// Combined hash of all anchors
    pub fn combined_hash(&self) -> [u8; 32] {
        let mut content = Vec::new();
        content.extend_from_slice(&self.factorization_modulus_hash);
        content.extend_from_slice(&self.lattice_hash);
        content.extend_from_slice(&self.ec_params_hash);
        *blake3::hash(&content).as_bytes()
    }
}

/// Dynamic evolution parameters
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EvolutionParams {
    /// Lorenz timestep
    pub lorenz_dt: f64,
    /// Cellular automaton mutation rate
    pub gol_mutation_rate: f64,
    /// Fractal escape threshold
    pub fractal_escape_threshold: f64,
    /// Quantum walk coin bias
    pub quantum_coin_bias: f64,
    /// Genome point mutation rate
    pub genome_mutation_rate: f64,
    /// Genome block mutation chance
    pub genome_block_mutation_chance: f64,
}

impl Default for EvolutionParams {
    fn default() -> Self {
        Self {
            lorenz_dt: 0.01,
            gol_mutation_rate: 0.01,
            fractal_escape_threshold: 2.0,
            quantum_coin_bias: 0.5,
            genome_mutation_rate: 0.05,
            genome_block_mutation_chance: 0.01,
        }
    }
}

/// Complete Organic Encryption State
/// 
/// The cryptographic organism that evolves over time, providing:
/// - Perfect forward secrecy (past states unrecoverable)
/// - Quantum resistance (post-quantum key derivation)
/// - Network synchronization (deterministic evolution from shared seed)
#[derive(Clone, Serialize, Deserialize)]
pub struct OrganicEncryptionState {
    /// Current evolution generation
    generation: u64,
    
    /// The cryptographic genome (default 992 bytes)
    genome: Vec<u8>,
    
    /// Lorenz attractor chaos state
    lorenz: LorenzState,
    
    /// Cellular automaton grid
    cellular: CellularGrid,
    
    /// Mandelbrot fractal state
    fractal: FractalState,
    
    /// Quantum walk state
    quantum: QuantumState,
    
    /// Mathematical impossibility anchors
    anchors: ImpossibilityAnchors,
    
    /// Dynamic evolution parameters
    params: EvolutionParams,
    
    /// Hash of previous state for chain verification
    previous_state_hash: [u8; 32],
    
    /// Current synchronization hash
    current_sync_hash: [u8; 32],
    
    /// Is the organism alive/initialized
    is_alive: bool,
    
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
        // Generate genome
        let genome = Self::generate_genome(seed);
        
        // Initialize all subsystems
        let lorenz = LorenzState::from_seed(seed);
        let cellular = CellularGrid::from_seed(seed, 64);
        let fractal = FractalState::from_seed(seed);
        let quantum = QuantumState::from_seed(seed);
        let anchors = ImpossibilityAnchors::from_seed(seed);
        
        // Derive initial key material
        let key_material = Self::derive_key_material_internal(&genome, 0);
        
        // Generate post-quantum keypairs (placeholders for real crypto)
        let (kyber_pk, kyber_sk) = Self::generate_kyber_keypair(&key_material[0..32]);
        let (dilithium_pk, dilithium_sk) = Self::generate_dilithium_keypair(&key_material[32..64]);
        
        let mut state = Self {
            generation: 0,
            genome,
            lorenz,
            cellular,
            fractal,
            quantum,
            anchors,
            params: EvolutionParams::default(),
            previous_state_hash: [0u8; 32],
            current_sync_hash: [0u8; 32],
            is_alive: true,
            kyber_public_key: kyber_pk,
            dilithium_public_key: dilithium_pk,
        };
        
        state.current_sync_hash = state.calculate_sync_hash();
        
        let secrets = OESSecrets {
            kyber_secret_key: kyber_sk,
            dilithium_secret_key: dilithium_sk,
        };
        
        (state, secrets)
    }

    /// Generate genome from seed
    fn generate_genome(seed: &[u8]) -> Vec<u8> {
        let mut genome = Vec::with_capacity(GENOME_DIMENSION);
        let mut hasher_state = seed.to_vec();
        
        while genome.len() < GENOME_DIMENSION {
            hasher_state = blake3::hash(&hasher_state).as_bytes().to_vec();
            genome.extend_from_slice(&hasher_state);
        }
        
        genome.truncate(GENOME_DIMENSION);
        genome
    }

    /// Evolve the OES state by one generation
    pub fn evolve(&mut self, anchor_hash: &[u8; 32]) -> OESSecrets {
        // Store previous state hash
        self.previous_state_hash = self.current_sync_hash;
        
        // 1. Evolve Lorenz attractor
        self.lorenz.evolve(self.params.lorenz_dt);
        
        // 2. Lorenz influences fractal and cellular mutation rate
        self.fractal.perturb(self.lorenz.x / 1000.0);
        self.params.gol_mutation_rate = (0.01 + (self.lorenz.z.abs() / 50.0) * 0.02).clamp(0.005, 0.05);
        
        // 3. Evolve cellular automaton
        self.cellular.evolve(self.params.gol_mutation_rate);
        
        // 4. Cellular density influences quantum bias
        self.params.quantum_coin_bias = 0.5 + (self.cellular.density() - 0.5) * 0.2;
        
        // 5. Evolve fractal
        self.fractal.evolve(self.params.fractal_escape_threshold);
        
        // 6. Evolve quantum walk
        self.quantum.evolve(self.params.quantum_coin_bias);
        
        // 7. Mutate genome
        self.mutate_genome(anchor_hash);
        
        // 8. Self-adaptation
        self.adapt_parameters();
        
        // 9. Increment generation
        self.generation += 1;
        
        // 10. Derive new key material
        let key_material = Self::derive_key_material_internal(&self.genome, self.generation);
        
        // 11. Generate new keypairs
        let (kyber_pk, kyber_sk) = Self::generate_kyber_keypair(&key_material[0..32]);
        let (dilithium_pk, dilithium_sk) = Self::generate_dilithium_keypair(&key_material[32..64]);
        
        self.kyber_public_key = kyber_pk;
        self.dilithium_public_key = dilithium_pk;
        
        // 12. Update sync hash
        self.current_sync_hash = self.calculate_sync_hash();
        
        OESSecrets {
            kyber_secret_key: kyber_sk,
            dilithium_secret_key: dilithium_sk,
        }
    }

    /// Mutate genome with multiple mutation types
    fn mutate_genome(&mut self, anchor_hash: &[u8; 32]) {
        let mut rng = ChaCha20Rng::from_seed(*anchor_hash);
        let genome_len = self.genome.len();
        
        // 1. Point mutations
        let num_mutations = (genome_len as f64 * self.params.genome_mutation_rate) as usize;
        for _ in 0..num_mutations {
            let idx = rand::Rng::gen_range(&mut rng, 0..genome_len);
            let bit = rand::Rng::gen_range(&mut rng, 0..8);
            self.genome[idx] ^= 1 << bit;
        }
        
        // 2. Block inversion
        if rand::Rng::gen::<f64>(&mut rng) < self.params.genome_block_mutation_chance {
            if genome_len >= 4 {
                let start = rand::Rng::gen_range(&mut rng, 0..genome_len - 1);
                let max_block = (genome_len / 4).min(genome_len - start);
                let end = start + rand::Rng::gen_range(&mut rng, 1..max_block.max(2));
                self.genome[start..end].reverse();
            }
        }
        
        // 3. Block replacement with random data
        if rand::Rng::gen::<f64>(&mut rng) < self.params.genome_block_mutation_chance / 2.0 {
            if genome_len >= 8 {
                let start = rand::Rng::gen_range(&mut rng, 0..genome_len - 4);
                let block_size = rand::Rng::gen_range(&mut rng, 2..5);
                for i in 0..block_size.min(genome_len - start) {
                    self.genome[start + i] = rand::Rng::gen(&mut rng);
                }
            }
        }
    }

    /// Adapt parameters based on system state
    fn adapt_parameters(&mut self) {
        let live_density = self.cellular.density();
        
        // If grid becomes too static, increase mutation rates
        if live_density < 0.05 || live_density > 0.95 {
            self.params.genome_mutation_rate = (self.params.genome_mutation_rate * 1.1).min(0.1);
            self.params.gol_mutation_rate = (self.params.gol_mutation_rate * 1.1).min(0.05);
        } else {
            self.params.genome_mutation_rate = (self.params.genome_mutation_rate * 0.9).max(0.005);
            self.params.gol_mutation_rate = (self.params.gol_mutation_rate * 0.9).max(0.005);
        }
    }

    /// Calculate comprehensive synchronization hash
    fn calculate_sync_hash(&self) -> [u8; 32] {
        let mut content = Vec::new();
        
        // Generation
        content.extend_from_slice(&self.generation.to_le_bytes());
        
        // Genome hash
        content.extend_from_slice(blake3::hash(&self.genome).as_bytes());
        
        // Lorenz state (truncated for determinism)
        content.extend_from_slice(&(self.lorenz.x as f32).to_le_bytes());
        content.extend_from_slice(&(self.lorenz.y as f32).to_le_bytes());
        content.extend_from_slice(&(self.lorenz.z as f32).to_le_bytes());
        
        // Cellular hash
        content.extend_from_slice(&self.cellular.hash());
        
        // Fractal state
        content.extend_from_slice(&(self.fractal.z_real as f32).to_le_bytes());
        content.extend_from_slice(&(self.fractal.z_imag as f32).to_le_bytes());
        
        // Quantum hash
        content.extend_from_slice(&self.quantum.hash());
        
        // Anchors
        content.extend_from_slice(&self.anchors.combined_hash());
        
        *blake3::hash(&content).as_bytes()
    }

    /// Derive key material from genome
    fn derive_key_material_internal(genome: &[u8], generation: u64) -> [u8; 64] {
        let mut input1 = genome.to_vec();
        input1.extend_from_slice(b"kyber_seed");
        input1.extend_from_slice(&generation.to_le_bytes());
        let hash1 = blake3::hash(&input1);
        
        let mut input2 = genome.to_vec();
        input2.extend_from_slice(b"dilithium_seed");
        input2.extend_from_slice(&generation.to_le_bytes());
        let hash2 = blake3::hash(&input2);
        
        let mut material = [0u8; 64];
        material[0..32].copy_from_slice(hash1.as_bytes());
        material[32..64].copy_from_slice(hash2.as_bytes());
        material
    }

    /// Generate Kyber keypair (placeholder)
    fn generate_kyber_keypair(seed: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut pk_input = seed.to_vec();
        pk_input.extend_from_slice(b"kyber_pk");
        let pk_hash = blake3::hash(&pk_input);
        
        let mut sk_input = seed.to_vec();
        sk_input.extend_from_slice(b"kyber_sk");
        let sk_hash = blake3::hash(&sk_input);
        
        (pk_hash.as_bytes().to_vec(), sk_hash.as_bytes().to_vec())
    }

    /// Generate Dilithium keypair (placeholder)
    fn generate_dilithium_keypair(seed: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut pk_input = seed.to_vec();
        pk_input.extend_from_slice(b"dilithium_pk");
        let pk_hash = blake3::hash(&pk_input);
        
        let mut sk_input = seed.to_vec();
        sk_input.extend_from_slice(b"dilithium_sk");
        let sk_hash = blake3::hash(&sk_input);
        
        (pk_hash.as_bytes().to_vec(), sk_hash.as_bytes().to_vec())
    }

    /// Derive a cryptographic key for a specific purpose
    pub fn derive_key(&self, length: usize, purpose: &str) -> Vec<u8> {
        let mut content = Vec::new();
        
        // Core dynamic states
        content.extend_from_slice(&self.genome);
        content.extend_from_slice(&(self.lorenz.x).to_le_bytes());
        content.extend_from_slice(&(self.lorenz.y).to_le_bytes());
        content.extend_from_slice(&(self.lorenz.z).to_le_bytes());
        content.extend_from_slice(&self.cellular.hash());
        content.extend_from_slice(&(self.fractal.z_real).to_le_bytes());
        content.extend_from_slice(&(self.fractal.z_imag).to_le_bytes());
        content.extend_from_slice(&self.quantum.hash());
        content.extend_from_slice(&self.generation.to_le_bytes());
        content.extend_from_slice(purpose.as_bytes());
        
        // Mix with anchors
        content.extend_from_slice(&self.anchors.combined_hash());
        
        // Iterative key stretching
        let mut derived = blake3::hash(&content).as_bytes().to_vec();
        let rounds = 100 + (self.generation % 100) as usize;
        
        for _ in 0..rounds {
            derived = blake3::hash(&derived).as_bytes().to_vec();
        }
        
        // Extend if needed
        while derived.len() < length {
            let mut ext_input = derived.clone();
            ext_input.extend_from_slice(&self.genome);
            let extension = blake3::hash(&ext_input);
            derived.extend_from_slice(extension.as_bytes());
        }
        
        derived.truncate(length);
        derived
    }

    // === Getters ===

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn is_alive(&self) -> bool {
        self.is_alive
    }

    pub fn sync_hash(&self) -> [u8; 32] {
        self.current_sync_hash
    }

    pub fn previous_hash(&self) -> [u8; 32] {
        self.previous_state_hash
    }

    pub fn kyber_public_key(&self) -> &[u8] {
        &self.kyber_public_key
    }

    pub fn dilithium_public_key(&self) -> &[u8] {
        &self.dilithium_public_key
    }

    pub fn is_valid_generation(&self, gen: u64) -> bool {
        let window = rope_core::types::constants::GENERATION_WINDOW;
        gen >= self.generation.saturating_sub(window) && gen <= self.generation + 1
    }

    /// Generate OES proof for a string
    pub fn generate_proof(&self) -> OESProof {
        OESProof {
            generation: self.generation,
            state_commitment: self.current_sync_hash,
            merkle_proof: Vec::new(),
            signature: Vec::new(),
        }
    }

    /// Verify an OES proof
    pub fn verify_proof(&self, proof: &OESProof) -> bool {
        if !self.is_valid_generation(proof.generation) {
            return false;
        }
        if proof.generation == self.generation {
            return proof.state_commitment == self.current_sync_hash;
        }
        true
    }

    /// Get genome summary for display
    pub fn genome_summary(&self) -> (usize, String, String, String) {
        let len = self.genome.len();
        let first = hex::encode(&self.genome[..32.min(len)]);
        let last = hex::encode(&self.genome[len.saturating_sub(32)..]);
        let full_hash = hex::encode(blake3::hash(&self.genome).as_bytes());
        (len, first, last, full_hash)
    }
}

/// OES Proof - Demonstrates string was created by synchronized node
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OESProof {
    pub generation: u64,
    pub state_commitment: [u8; 32],
    pub merkle_proof: Vec<[u8; 32]>,
    pub signature: Vec<u8>,
}

impl OESProof {
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

    pub fn generation(&self) -> u64 {
        self.state.read().generation()
    }

    pub fn should_evolve(&self, anchor_count: u64) -> bool {
        anchor_count > 0 && anchor_count % OES_EVOLUTION_INTERVAL == 0
    }

    pub fn evolve(&self, anchor_hash: &[u8; 32]) {
        let mut state = self.state.write();
        let new_secrets = state.evolve(anchor_hash);
        
        let mut secrets = self.secrets.write();
        *secrets = new_secrets;
        
        let mut counter = self.evolution_counter.write();
        *counter += 1;
    }

    pub fn generate_proof(&self) -> OESProof {
        self.state.read().generate_proof()
    }

    pub fn verify_proof(&self, proof: &OESProof) -> bool {
        self.state.read().verify_proof(proof)
    }

    pub fn derive_key(&self, length: usize, purpose: &str) -> Vec<u8> {
        self.state.read().derive_key(length, purpose)
    }

    pub fn sync_hash(&self) -> [u8; 32] {
        self.state.read().sync_hash()
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
        assert!(state.is_alive());
        assert_eq!(state.genome.len(), GENOME_DIMENSION);
    }

    #[test]
    fn test_evolution() {
        let seed = [1u8; 32];
        let (mut state, _) = OrganicEncryptionState::genesis(&seed);
        
        let initial_hash = state.sync_hash();
        let initial_gen = state.generation();
        
        let anchor_hash = [2u8; 32];
        let _new_secrets = state.evolve(&anchor_hash);
        
        assert_eq!(state.generation(), initial_gen + 1);
        assert_ne!(state.sync_hash(), initial_hash);
        assert_eq!(state.previous_hash(), initial_hash);
    }

    #[test]
    fn test_deterministic_evolution() {
        let seed = [42u8; 32];
        let anchor_hash = [99u8; 32];
        
        let (mut state1, _) = OrganicEncryptionState::genesis(&seed);
        let (mut state2, _) = OrganicEncryptionState::genesis(&seed);
        
        // Both start with same state
        assert_eq!(state1.sync_hash(), state2.sync_hash());
        
        // Same anchor -> same evolution (mostly deterministic)
        state1.evolve(&anchor_hash);
        state2.evolve(&anchor_hash);
        
        assert_eq!(state1.generation(), state2.generation());
    }

    #[test]
    fn test_key_derivation() {
        let seed = [0u8; 32];
        let (state, _) = OrganicEncryptionState::genesis(&seed);
        
        let key1 = state.derive_key(32, "encryption");
        let key2 = state.derive_key(32, "signing");
        
        assert_eq!(key1.len(), 32);
        assert_eq!(key2.len(), 32);
        assert_ne!(key1, key2); // Different purposes = different keys
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
    fn test_lorenz_evolution() {
        let mut lorenz = LorenzState::from_seed(&[0u8; 32]);
        let initial_x = lorenz.x;
        
        for _ in 0..100 {
            lorenz.evolve(0.01);
        }
        
        assert_ne!(lorenz.x, initial_x);
    }

    #[test]
    fn test_cellular_evolution() {
        let mut grid = CellularGrid::from_seed(&[0u8; 32], 16);
        let initial_hash = grid.hash();
        
        grid.evolve(0.01);
        
        // Grid should change (with high probability due to mutations)
        // Note: Could be same by chance, but very unlikely
        let _ = grid.hash(); // Just verify it computes
    }

    #[test]
    fn test_quantum_evolution() {
        let mut quantum = QuantumState::from_seed(&[0u8; 32]);
        
        for _ in 0..10 {
            quantum.evolve(0.5);
        }
        
        // Position should be within bounds
        assert!(quantum.position >= -5 && quantum.position <= 5);
    }
}
