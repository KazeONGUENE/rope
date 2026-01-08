//! Full System Benchmarks for Datachain Rope
//!
//! Run with: `cargo bench --package rope-benchmarks --bench full_system_benchmarks`

use criterion::{criterion_group, criterion_main, Criterion, BenchmarkId, Throughput};
use std::time::Duration;

// ============================================================================
// CRYPTO BENCHMARKS
// ============================================================================

fn crypto_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("crypto");
    group.measurement_time(Duration::from_secs(10));
    
    // OES Key Generation
    group.bench_function("oes_keygen", |b| {
        b.iter(|| {
            let seed = [0u8; 32];
            blake3::hash(&seed)
        });
    });
    
    // Dilithium3 Signing (simulated)
    let message = vec![0u8; 256];
    group.bench_function("dilithium_sign", |b| {
        b.iter(|| {
            blake3::keyed_hash(&[0u8; 32], &message)
        });
    });
    
    // Kyber768 Encapsulation (simulated)
    group.bench_function("kyber_encap", |b| {
        b.iter(|| {
            blake3::hash(&[0u8; 32])
        });
    });
    
    // Hybrid signature
    group.bench_function("hybrid_signature", |b| {
        let msg = vec![0u8; 256];
        b.iter(|| {
            let _ed = blake3::hash(&msg);
            let _dil = blake3::keyed_hash(&[0u8; 32], &msg);
        });
    });
    
    group.finish();
}

// ============================================================================
// STRING BENCHMARKS
// ============================================================================

fn string_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("strings");
    group.measurement_time(Duration::from_secs(10));
    
    // String creation with varying payload sizes
    for size in [256, 1024, 4096, 16384].iter() {
        let payload = vec![0u8; *size];
        
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("creation", size),
            &payload,
            |b, payload| {
                b.iter(|| {
                    let _id = blake3::hash(payload);
                    let _sig = blake3::keyed_hash(&[0u8; 32], payload);
                });
            },
        );
    }
    
    // String validation
    let payload = vec![0u8; 1024];
    let expected_hash = blake3::hash(&payload);
    group.bench_function("validation", |b| {
        b.iter(|| {
            blake3::hash(&payload) == expected_hash
        });
    });
    
    // Lattice insertion (simulated)
    group.bench_function("lattice_insertion", |b| {
        b.iter(|| {
            let _id = blake3::hash(&rand::random::<[u8; 32]>());
        });
    });
    
    group.finish();
}

// ============================================================================
// CONSENSUS BENCHMARKS
// ============================================================================

fn consensus_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("consensus");
    group.measurement_time(Duration::from_secs(10));
    
    // Virtual voting with varying validator counts
    for validator_count in [7, 21, 51].iter() {
        group.bench_with_input(
            BenchmarkId::new("virtual_voting", validator_count),
            validator_count,
            |b, &count| {
                b.iter(|| {
                    for _ in 0..count {
                        let _vote = blake3::hash(&rand::random::<[u8; 32]>());
                    }
                });
            },
        );
    }
    
    // Testimony creation
    group.bench_function("testimony_creation", |b| {
        b.iter(|| {
            let _testimony = blake3::hash(&rand::random::<[u8; 64]>());
        });
    });
    
    // Anchor determination
    for string_count in [100, 1000, 10000].iter() {
        group.bench_with_input(
            BenchmarkId::new("anchor_determination", string_count),
            string_count,
            |b, &count| {
                b.iter(|| {
                    let mut anchor = [0u8; 32];
                    for _ in 0..count {
                        anchor = *blake3::hash(&anchor).as_bytes();
                    }
                    anchor
                });
            },
        );
    }
    
    group.finish();
}

// ============================================================================
// NETWORK BENCHMARKS
// ============================================================================

fn network_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("network");
    group.measurement_time(Duration::from_secs(10));
    
    // Message serialization
    for size in [256, 1024, 4096].iter() {
        let message = vec![0u8; *size];
        
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("serialize", size),
            &message,
            |b, msg| {
                b.iter(|| {
                    bincode::serialize(msg).unwrap()
                });
            },
        );
    }
    
    // Message deserialization
    for size in [256, 1024, 4096].iter() {
        let message = vec![0u8; *size];
        let encoded = bincode::serialize(&message).unwrap();
        
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(
            BenchmarkId::new("deserialize", size),
            &encoded,
            |b, enc| {
                b.iter(|| {
                    let _decoded: Vec<u8> = bincode::deserialize(enc).unwrap();
                });
            },
        );
    }
    
    // Gossip propagation simulation
    for peer_count in [10, 50, 100].iter() {
        let message = vec![0u8; 256];
        
        group.bench_with_input(
            BenchmarkId::new("gossip_sim", peer_count),
            peer_count,
            |b, &count| {
                b.iter(|| {
                    for _ in 0..count {
                        let _hash = blake3::hash(&message);
                    }
                });
            },
        );
    }
    
    group.finish();
}

// ============================================================================
// PROTOCOL BENCHMARKS
// ============================================================================

fn protocol_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("protocols");
    group.measurement_time(Duration::from_secs(10));
    
    // Reed-Solomon encoding (simulated)
    for size_kb in [64, 256, 1024].iter() {
        let data = vec![0u8; size_kb * 1024];
        
        group.throughput(Throughput::Bytes((size_kb * 1024) as u64));
        group.bench_with_input(
            BenchmarkId::new("rs_encode", format!("{}KB", size_kb)),
            &data,
            |b, data| {
                b.iter(|| {
                    // Simulate RS encoding
                    blake3::hash(data)
                });
            },
        );
    }
    
    // Reed-Solomon decoding (simulated)
    for size_kb in [64, 256, 1024].iter() {
        let data = vec![0u8; size_kb * 1024];
        
        group.throughput(Throughput::Bytes((size_kb * 1024) as u64));
        group.bench_with_input(
            BenchmarkId::new("rs_decode", format!("{}KB", size_kb)),
            &data,
            |b, data| {
                b.iter(|| {
                    // Simulate RS decoding
                    blake3::hash(data)
                });
            },
        );
    }
    
    group.finish();
}

// ============================================================================
// THROUGHPUT BENCHMARKS
// ============================================================================

fn throughput_benchmarks(c: &mut Criterion) {
    let mut group = c.benchmark_group("throughput");
    group.measurement_time(Duration::from_secs(15));
    group.sample_size(50);
    
    // Simulate full transaction throughput
    group.throughput(Throughput::Elements(1000));
    group.bench_function("transactions_1000", |b| {
        b.iter(|| {
            for _ in 0..1000 {
                let _tx_id = blake3::hash(&rand::random::<[u8; 256]>());
            }
        });
    });
    
    // Network message throughput
    group.throughput(Throughput::Bytes(1_000_000));
    group.bench_function("network_1MB", |b| {
        let data = vec![0u8; 1_000_000];
        b.iter(|| {
            blake3::hash(&data)
        });
    });
    
    group.finish();
}

criterion_group!(
    benches,
    crypto_benchmarks,
    string_benchmarks,
    consensus_benchmarks,
    network_benchmarks,
    protocol_benchmarks,
    throughput_benchmarks,
);
criterion_main!(benches);

