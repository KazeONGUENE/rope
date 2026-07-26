//! Quipu Canon v2.0 Phase 5 — PQ signing offload benchmark.
//!
//! Measures hybrid (Ed25519 + Dilithium3) signature production three ways:
//!
//! 1. `serial`   — one thread, inline `HybridSigner::sign` (the pre-Phase-5
//!                 consensus hot path).
//! 2. `pool`     — `CpuPoolBackend::sign_batch` (rayon data-parallel).
//! 3. `pipeline` — full `OffloadSigner` queue → batcher → pool path,
//!                 including ticketing overhead.
//!
//! Run: `cargo run --release -p rope-crypto --example offload_bench [N]`
//! (default N = 2048 messages).

use rope_crypto::hybrid::{HybridSigner, HybridVerifier};
use rope_crypto::offload::{CpuPoolBackend, OffloadSigner, SigningBackend};
use std::sync::Arc;
use std::time::Instant;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(2048);

    let cores = std::thread::available_parallelism()
        .map(|c| c.get())
        .unwrap_or(1);
    println!("PQ signing offload benchmark — {n} messages, {cores} cores");
    println!("scheme: hybrid Ed25519 + CRYSTALS-Dilithium3 (NIST PQ-3)\n");

    let (signer, pk) = HybridSigner::generate();
    let signer = Arc::new(signer);
    let messages: Vec<Vec<u8>> = (0..n)
        .map(|i| format!("phase5-bench-testimony-{i}").into_bytes())
        .collect();

    // ---- 1. Serial baseline -------------------------------------------
    // Warm-up.
    let _ = signer.sign(&messages[0]);
    let t = Instant::now();
    let serial_sigs: Vec<_> = messages.iter().map(|m| signer.sign(m)).collect();
    let serial = t.elapsed();
    report("serial (hot path today)", n, serial);

    // ---- 2. CPU pool backend ------------------------------------------
    let backend = CpuPoolBackend::new(signer.clone());
    let _ = backend.sign_batch(&messages[..cores.min(n)].to_vec()); // warm-up rayon
    let t = Instant::now();
    let pool_sigs = backend.sign_batch(&messages);
    let pool = t.elapsed();
    report("cpu pool (rayon batch)", n, pool);

    // ---- 3. Full pipeline ---------------------------------------------
    let pipeline = OffloadSigner::start_cpu(signer.clone());
    // Warm-up.
    let _ = pipeline
        .sign_batch_blocking(messages[..cores.min(n)].to_vec())
        .unwrap();
    let t = Instant::now();
    let pipe_sigs = pipeline.sign_batch_blocking(messages.clone()).unwrap();
    let pipe = t.elapsed();
    report("offload pipeline (queue+batch)", n, pipe);

    let stats = pipeline.stats();
    println!(
        "\npipeline stats: backend={} batches={} mean_batch={:.1} queue_high_water={}",
        stats.backend, stats.batches, stats.mean_batch_size, stats.queue_high_water
    );

    // ---- Correctness gate (never benchmark theatre) --------------------
    print!("\nverifying all {} signatures from all three paths... ", 3 * n);
    for (m, s) in messages
        .iter()
        .zip(serial_sigs.iter())
        .chain(messages.iter().zip(pool_sigs.iter()))
        .chain(messages.iter().zip(pipe_sigs.iter()))
    {
        assert!(
            HybridVerifier::verify(&pk, m, s).expect("verify must not error"),
            "benchmark produced an invalid signature — abort"
        );
    }
    println!("OK");

    let speedup = serial.as_secs_f64() / pool.as_secs_f64();
    println!(
        "\nsummary: pool speedup {speedup:.2}× over serial on {cores} cores \
         ({:.0} sig/s serial → {:.0} sig/s pool)",
        n as f64 / serial.as_secs_f64(),
        n as f64 / pool.as_secs_f64(),
    );
}

fn report(label: &str, n: usize, d: std::time::Duration) {
    println!(
        "{label:<32} {:>10.2} ms   {:>10.0} sig/s   {:>8.1} µs/sig",
        d.as_secs_f64() * 1e3,
        n as f64 / d.as_secs_f64(),
        d.as_secs_f64() * 1e6 / n as f64
    );
}
