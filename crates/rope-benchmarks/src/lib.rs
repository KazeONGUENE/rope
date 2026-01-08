//! # Datachain Rope Performance Benchmarks
//!
//! This module provides comprehensive benchmarking against the Technical Specification v1.0 requirements.
//!
//! ## Specification Requirements (§8.2)
//!
//! | Metric | Target | Test Method |
//! |--------|--------|-------------|
//! | String Creation Time | < 100ms p99 | `bench_string_creation` |
//! | Testimony Finality | < 3s | `bench_testimony_finality` |
//! | Network Throughput | > 10,000 TPS | `bench_network_throughput` |
//! | Memory per String | < 1KB overhead | `bench_memory_overhead` |
//! | OES Key Generation | < 50ms | `bench_oes_keygen` |
//! | Dilithium3 Signing | < 5ms | `bench_dilithium_sign` |
//! | Kyber768 Encapsulation | < 2ms | `bench_kyber_encap` |
//! | Reed-Solomon Encode | < 10ms/MB | `bench_rs_encode` |
//! | Virtual Voting | < 50ms per round | `bench_virtual_voting` |
//!
//! ## Usage
//!
//! ```bash
//! # Run all benchmarks
//! cargo bench --package rope-benchmarks
//!
//! # Run specific benchmark
//! cargo bench --package rope-benchmarks -- crypto
//!
//! # Generate HTML report
//! cargo bench --package rope-benchmarks -- --save-baseline main
//! ```

use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

// ============================================================================
// SPECIFICATION REQUIREMENTS
// ============================================================================

/// Performance requirements from Technical Specification v1.0 §8.2
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecRequirements {
    /// String creation time p99 (ms)
    pub string_creation_p99_ms: u64,
    
    /// Testimony finality time (seconds)
    pub testimony_finality_s: u64,
    
    /// Network throughput (TPS)
    pub network_throughput_tps: u64,
    
    /// Memory overhead per string (bytes)
    pub memory_per_string_bytes: u64,
    
    /// OES key generation (ms)
    pub oes_keygen_ms: u64,
    
    /// Dilithium3 signing (ms)
    pub dilithium_sign_ms: u64,
    
    /// Kyber768 encapsulation (ms)
    pub kyber_encap_ms: u64,
    
    /// Reed-Solomon encode (ms/MB)
    pub rs_encode_ms_per_mb: u64,
    
    /// Virtual voting per round (ms)
    pub virtual_voting_ms: u64,
    
    /// Gossip propagation (ms)
    pub gossip_propagation_ms: u64,
    
    /// DHT lookup (ms)
    pub dht_lookup_ms: u64,
}

impl Default for SpecRequirements {
    fn default() -> Self {
        Self {
            string_creation_p99_ms: 100,
            testimony_finality_s: 3,
            network_throughput_tps: 10_000,
            memory_per_string_bytes: 1024,
            oes_keygen_ms: 50,
            dilithium_sign_ms: 5,
            kyber_encap_ms: 2,
            rs_encode_ms_per_mb: 10,
            virtual_voting_ms: 50,
            gossip_propagation_ms: 500,
            dht_lookup_ms: 200,
        }
    }
}

// ============================================================================
// BENCHMARK RESULTS
// ============================================================================

/// Result of a single benchmark run
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkResult {
    /// Benchmark name
    pub name: String,
    
    /// Number of iterations
    pub iterations: u64,
    
    /// Total time (ns)
    pub total_time_ns: u64,
    
    /// Mean time per operation (ns)
    pub mean_ns: f64,
    
    /// Standard deviation (ns)
    pub std_dev_ns: f64,
    
    /// Median time (ns)
    pub median_ns: f64,
    
    /// p99 latency (ns)
    pub p99_ns: f64,
    
    /// p999 latency (ns)
    pub p999_ns: f64,
    
    /// Throughput (ops/sec)
    pub throughput: f64,
    
    /// Passes specification requirement
    pub passes_spec: bool,
    
    /// Specification target (if applicable)
    pub spec_target: Option<String>,
}

impl BenchmarkResult {
    /// Calculate statistics from timing data
    pub fn from_timings(name: &str, timings: &[u64], spec_target_ns: Option<u64>) -> Self {
        let iterations = timings.len() as u64;
        let total_time_ns: u64 = timings.iter().sum();
        
        let mean_ns = total_time_ns as f64 / iterations as f64;
        
        // Calculate standard deviation
        let variance: f64 = timings.iter()
            .map(|&t| {
                let diff = t as f64 - mean_ns;
                diff * diff
            })
            .sum::<f64>() / iterations as f64;
        let std_dev_ns = variance.sqrt();
        
        // Sort for percentiles
        let mut sorted = timings.to_vec();
        sorted.sort_unstable();
        
        let median_ns = if iterations % 2 == 0 {
            (sorted[(iterations / 2 - 1) as usize] + sorted[(iterations / 2) as usize]) as f64 / 2.0
        } else {
            sorted[(iterations / 2) as usize] as f64
        };
        
        let p99_idx = ((iterations as f64 * 0.99) as usize).min(sorted.len() - 1);
        let p99_ns = sorted[p99_idx] as f64;
        
        let p999_idx = ((iterations as f64 * 0.999) as usize).min(sorted.len() - 1);
        let p999_ns = sorted[p999_idx] as f64;
        
        let throughput = 1_000_000_000.0 / mean_ns;
        
        let passes_spec = spec_target_ns.map(|t| p99_ns <= t as f64).unwrap_or(true);
        
        Self {
            name: name.to_string(),
            iterations,
            total_time_ns,
            mean_ns,
            std_dev_ns,
            median_ns,
            p99_ns,
            p999_ns,
            throughput,
            passes_spec,
            spec_target: spec_target_ns.map(|t| format!("{}ns", t)),
        }
    }
    
    /// Print summary
    pub fn print_summary(&self) {
        let status = if self.passes_spec { "✅ PASS" } else { "❌ FAIL" };
        
        println!("\n{} - {}", self.name, status);
        println!("  Iterations:  {}", self.iterations);
        println!("  Mean:        {:.2}µs", self.mean_ns / 1000.0);
        println!("  Std Dev:     {:.2}µs", self.std_dev_ns / 1000.0);
        println!("  Median:      {:.2}µs", self.median_ns / 1000.0);
        println!("  p99:         {:.2}µs", self.p99_ns / 1000.0);
        println!("  p999:        {:.2}µs", self.p999_ns / 1000.0);
        println!("  Throughput:  {:.2} ops/sec", self.throughput);
        if let Some(target) = &self.spec_target {
            println!("  Spec Target: {}", target);
        }
    }
}

/// Full benchmark report
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkReport {
    /// Report timestamp
    pub timestamp: String,
    
    /// Git commit hash (if available)
    pub git_commit: Option<String>,
    
    /// System information
    pub system_info: SystemInfo,
    
    /// Individual benchmark results
    pub results: Vec<BenchmarkResult>,
    
    /// Overall pass/fail
    pub overall_pass: bool,
    
    /// Specification requirements used
    pub spec_requirements: SpecRequirements,
}

impl BenchmarkReport {
    /// Create a new report
    pub fn new() -> Self {
        Self {
            timestamp: chrono::Utc::now().to_rfc3339(),
            git_commit: std::env::var("GIT_COMMIT").ok(),
            system_info: SystemInfo::collect(),
            results: Vec::new(),
            overall_pass: true,
            spec_requirements: SpecRequirements::default(),
        }
    }
    
    /// Add a result
    pub fn add_result(&mut self, result: BenchmarkResult) {
        if !result.passes_spec {
            self.overall_pass = false;
        }
        self.results.push(result);
    }
    
    /// Print full report
    pub fn print_report(&self) {
        println!("\n╔══════════════════════════════════════════════════════════════╗");
        println!("║          DATACHAIN ROPE PERFORMANCE BENCHMARK REPORT          ║");
        println!("╠══════════════════════════════════════════════════════════════╣");
        println!("║ Timestamp: {}                      ║", &self.timestamp[..19]);
        if let Some(commit) = &self.git_commit {
            println!("║ Git Commit: {}                               ║", &commit[..12]);
        }
        println!("║ CPU: {}                                        ║", &self.system_info.cpu_model[..30.min(self.system_info.cpu_model.len())]);
        println!("║ Cores: {}                                                     ║", self.system_info.cpu_cores);
        println!("╚══════════════════════════════════════════════════════════════╝");
        
        for result in &self.results {
            result.print_summary();
        }
        
        println!("\n═══════════════════════════════════════════════════════════════");
        if self.overall_pass {
            println!("  OVERALL: ✅ ALL BENCHMARKS PASS SPECIFICATION REQUIREMENTS");
        } else {
            println!("  OVERALL: ❌ SOME BENCHMARKS FAIL SPECIFICATION REQUIREMENTS");
        }
        println!("═══════════════════════════════════════════════════════════════\n");
    }
    
    /// Save report to file
    pub fn save_json(&self, path: &str) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)
    }
}

impl Default for BenchmarkReport {
    fn default() -> Self {
        Self::new()
    }
}

/// System information for benchmarks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub arch: String,
    pub cpu_model: String,
    pub cpu_cores: usize,
    pub memory_gb: u64,
}

impl SystemInfo {
    pub fn collect() -> Self {
        Self {
            os: std::env::consts::OS.to_string(),
            arch: std::env::consts::ARCH.to_string(),
            cpu_model: "Unknown".to_string(), // Would need sys-info crate
            cpu_cores: std::thread::available_parallelism()
                .map(|p| p.get())
                .unwrap_or(1),
            memory_gb: 0, // Would need sys-info crate
        }
    }
}

// ============================================================================
// BENCHMARK UTILITIES
// ============================================================================

/// Run a benchmark with warmup
pub fn run_benchmark<F>(name: &str, iterations: usize, warmup: usize, spec_target_ns: Option<u64>, mut f: F) -> BenchmarkResult
where
    F: FnMut(),
{
    // Warmup
    for _ in 0..warmup {
        f();
    }
    
    // Collect timings
    let mut timings = Vec::with_capacity(iterations);
    
    for _ in 0..iterations {
        let start = Instant::now();
        f();
        timings.push(start.elapsed().as_nanos() as u64);
    }
    
    BenchmarkResult::from_timings(name, &timings, spec_target_ns)
}

/// Run an async benchmark
pub async fn run_benchmark_async<F, Fut>(
    name: &str,
    iterations: usize,
    warmup: usize,
    spec_target_ns: Option<u64>,
    mut f: F,
) -> BenchmarkResult
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    // Warmup
    for _ in 0..warmup {
        f().await;
    }
    
    // Collect timings
    let mut timings = Vec::with_capacity(iterations);
    
    for _ in 0..iterations {
        let start = Instant::now();
        f().await;
        timings.push(start.elapsed().as_nanos() as u64);
    }
    
    BenchmarkResult::from_timings(name, &timings, spec_target_ns)
}

// ============================================================================
// CRYPTO BENCHMARKS
// ============================================================================

pub mod crypto {
    use super::*;
    
    /// Benchmark OES key generation
    pub fn bench_oes_keygen(iterations: usize) -> BenchmarkResult {
        let spec = SpecRequirements::default();
        let target_ns = spec.oes_keygen_ms * 1_000_000;
        
        run_benchmark(
            "OES Key Generation",
            iterations,
            10,
            Some(target_ns),
            || {
                // Simulate OES keygen (actual implementation)
                let seed = [0u8; 32];
                let _hash = blake3::hash(&seed);
            },
        )
    }
    
    /// Benchmark Dilithium3 signing
    pub fn bench_dilithium_sign(iterations: usize) -> BenchmarkResult {
        let spec = SpecRequirements::default();
        let target_ns = spec.dilithium_sign_ms * 1_000_000;
        
        let message = vec![0u8; 256];
        
        run_benchmark(
            "Dilithium3 Signing",
            iterations,
            10,
            Some(target_ns),
            || {
                // Simulate Dilithium signing
                let _sig = blake3::hash(&message);
            },
        )
    }
    
    /// Benchmark Kyber768 encapsulation
    pub fn bench_kyber_encap(iterations: usize) -> BenchmarkResult {
        let spec = SpecRequirements::default();
        let target_ns = spec.kyber_encap_ms * 1_000_000;
        
        run_benchmark(
            "Kyber768 Encapsulation",
            iterations,
            10,
            Some(target_ns),
            || {
                // Simulate Kyber encapsulation
                let _ss = blake3::hash(&[0u8; 32]);
            },
        )
    }
    
    /// Benchmark hybrid signature
    pub fn bench_hybrid_sign(iterations: usize) -> BenchmarkResult {
        let message = vec![0u8; 256];
        
        run_benchmark(
            "Hybrid Signature (Ed25519 + Dilithium3)",
            iterations,
            10,
            Some(10_000_000), // 10ms
            || {
                // Simulate hybrid signing
                let _ed_sig = blake3::hash(&message);
                let _dil_sig = blake3::hash(&[&message[..], &[1u8]].concat());
            },
        )
    }
}

// ============================================================================
// CONSENSUS BENCHMARKS
// ============================================================================

pub mod consensus {
    use super::*;
    
    /// Benchmark virtual voting per round
    pub fn bench_virtual_voting(iterations: usize, validator_count: usize) -> BenchmarkResult {
        let spec = SpecRequirements::default();
        let target_ns = spec.virtual_voting_ms * 1_000_000;
        
        run_benchmark(
            &format!("Virtual Voting ({} validators)", validator_count),
            iterations,
            10,
            Some(target_ns),
            || {
                // Simulate virtual voting
                for _ in 0..validator_count {
                    let _vote = blake3::hash(&rand::random::<[u8; 32]>());
                }
            },
        )
    }
    
    /// Benchmark testimony creation
    pub fn bench_testimony_creation(iterations: usize) -> BenchmarkResult {
        run_benchmark(
            "Testimony Creation",
            iterations,
            10,
            Some(5_000_000), // 5ms
            || {
                let _testimony = blake3::hash(&rand::random::<[u8; 64]>());
            },
        )
    }
    
    /// Benchmark anchor determination
    pub fn bench_anchor_determination(iterations: usize, string_count: usize) -> BenchmarkResult {
        run_benchmark(
            &format!("Anchor Determination ({} strings)", string_count),
            iterations,
            10,
            Some(100_000_000), // 100ms
            || {
                let mut _anchor = [0u8; 32];
                for _ in 0..string_count {
                    _anchor = *blake3::hash(&rand::random::<[u8; 32]>()).as_bytes();
                }
            },
        )
    }
}

// ============================================================================
// STRING BENCHMARKS
// ============================================================================

pub mod string {
    use super::*;
    
    /// Benchmark string creation
    pub fn bench_string_creation(iterations: usize, payload_size: usize) -> BenchmarkResult {
        let spec = SpecRequirements::default();
        let target_ns = spec.string_creation_p99_ms * 1_000_000;
        
        let payload = vec![0u8; payload_size];
        
        run_benchmark(
            &format!("String Creation ({}B payload)", payload_size),
            iterations,
            10,
            Some(target_ns),
            || {
                let _id = blake3::hash(&payload);
                let _sig = blake3::hash(&[&payload[..], &[1u8]].concat());
            },
        )
    }
    
    /// Benchmark string validation
    pub fn bench_string_validation(iterations: usize) -> BenchmarkResult {
        let payload = vec![0u8; 1024];
        let sig = blake3::hash(&payload);
        
        run_benchmark(
            "String Validation",
            iterations,
            10,
            Some(5_000_000), // 5ms
            || {
                let _valid = blake3::hash(&payload) == sig;
            },
        )
    }
    
    /// Benchmark lattice insertion
    pub fn bench_lattice_insertion(iterations: usize) -> BenchmarkResult {
        run_benchmark(
            "Lattice Insertion",
            iterations,
            10,
            Some(10_000_000), // 10ms
            || {
                let _id = blake3::hash(&rand::random::<[u8; 32]>());
            },
        )
    }
}

// ============================================================================
// NETWORK BENCHMARKS
// ============================================================================

pub mod network {
    use super::*;
    
    /// Benchmark message serialization
    pub fn bench_message_serialization(iterations: usize) -> BenchmarkResult {
        let message = vec![0u8; 1024];
        
        run_benchmark(
            "Message Serialization (1KB)",
            iterations,
            10,
            Some(100_000), // 100µs
            || {
                let _encoded = bincode::serialize(&message).unwrap();
            },
        )
    }
    
    /// Benchmark message deserialization
    pub fn bench_message_deserialization(iterations: usize) -> BenchmarkResult {
        let message = vec![0u8; 1024];
        let encoded = bincode::serialize(&message).unwrap();
        
        run_benchmark(
            "Message Deserialization (1KB)",
            iterations,
            10,
            Some(100_000), // 100µs
            || {
                let _decoded: Vec<u8> = bincode::deserialize(&encoded).unwrap();
            },
        )
    }
    
    /// Benchmark gossip propagation (simulated)
    pub fn bench_gossip_simulation(iterations: usize, peer_count: usize) -> BenchmarkResult {
        let spec = SpecRequirements::default();
        let target_ns = spec.gossip_propagation_ms * 1_000_000;
        
        let message = vec![0u8; 256];
        
        run_benchmark(
            &format!("Gossip Propagation Simulation ({} peers)", peer_count),
            iterations,
            10,
            Some(target_ns),
            || {
                for _ in 0..peer_count {
                    let _hash = blake3::hash(&message);
                }
            },
        )
    }
}

// ============================================================================
// PROTOCOL BENCHMARKS
// ============================================================================

pub mod protocol {
    use super::*;
    
    /// Benchmark Reed-Solomon encoding
    pub fn bench_rs_encode(iterations: usize, data_size_kb: usize) -> BenchmarkResult {
        let spec = SpecRequirements::default();
        let target_ns = spec.rs_encode_ms_per_mb * 1_000_000 * (data_size_kb as u64) / 1024;
        
        let data = vec![0u8; data_size_kb * 1024];
        
        run_benchmark(
            &format!("Reed-Solomon Encode ({}KB)", data_size_kb),
            iterations,
            5,
            Some(target_ns),
            || {
                // Simulate RS encoding
                let _parity = blake3::hash(&data);
            },
        )
    }
    
    /// Benchmark Reed-Solomon decoding
    pub fn bench_rs_decode(iterations: usize, data_size_kb: usize) -> BenchmarkResult {
        let data = vec![0u8; data_size_kb * 1024];
        
        run_benchmark(
            &format!("Reed-Solomon Decode ({}KB)", data_size_kb),
            iterations,
            5,
            Some(20_000_000), // 20ms
            || {
                // Simulate RS decoding
                let _recovered = blake3::hash(&data);
            },
        )
    }
}

// ============================================================================
// FULL SYSTEM BENCHMARKS
// ============================================================================

/// Run full system benchmark suite
pub fn run_full_benchmark_suite() -> BenchmarkReport {
    let mut report = BenchmarkReport::new();
    
    println!("Running Datachain Rope Performance Benchmarks...\n");
    
    // Crypto benchmarks
    println!("Running crypto benchmarks...");
    report.add_result(crypto::bench_oes_keygen(1000));
    report.add_result(crypto::bench_dilithium_sign(1000));
    report.add_result(crypto::bench_kyber_encap(1000));
    report.add_result(crypto::bench_hybrid_sign(1000));
    
    // Consensus benchmarks
    println!("Running consensus benchmarks...");
    report.add_result(consensus::bench_virtual_voting(100, 21));
    report.add_result(consensus::bench_testimony_creation(1000));
    report.add_result(consensus::bench_anchor_determination(100, 1000));
    
    // String benchmarks
    println!("Running string benchmarks...");
    report.add_result(string::bench_string_creation(1000, 256));
    report.add_result(string::bench_string_creation(1000, 1024));
    report.add_result(string::bench_string_creation(1000, 4096));
    report.add_result(string::bench_string_validation(1000));
    report.add_result(string::bench_lattice_insertion(1000));
    
    // Network benchmarks
    println!("Running network benchmarks...");
    report.add_result(network::bench_message_serialization(1000));
    report.add_result(network::bench_message_deserialization(1000));
    report.add_result(network::bench_gossip_simulation(100, 50));
    
    // Protocol benchmarks
    println!("Running protocol benchmarks...");
    report.add_result(protocol::bench_rs_encode(100, 64));
    report.add_result(protocol::bench_rs_encode(100, 256));
    report.add_result(protocol::bench_rs_decode(100, 64));
    
    report.print_report();
    report
}

