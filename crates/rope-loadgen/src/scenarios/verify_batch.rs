//! `verify-batch` — Quipu Canon v2.0 Phase 2.C microbenchmark.
//!
//! Generates `--items` hybrid-signed payloads (Ed25519 + Dilithium3),
//! then alternately measures:
//!
//! 1. The serial path: a loop of `HybridVerifier::verify(...)` once
//!    per item, single-threaded.
//! 2. The batch path: a single `HybridVerifier::verify_batch(...)`
//!    call, which dispatches across the rayon worker pool and reuses
//!    the parsed-PK cache.
//!
//! Both paths verify the same items in the same order, so the only
//! variable is the dispatch strategy. The reported speedup is
//! `serial_elapsed / batch_elapsed`, averaged over `--iterations`
//! warmup-stripped runs.
//!
//! Output is the standard `Report::VerifyBatch` JSON on stdout plus a
//! human summary on stderr.

use crate::cli::VerifyBatchArgs;
use crate::report::{Report, VerifyBatchReport};
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rope_crypto::batch::BatchVerifyItem;
use rope_crypto::hybrid::{HybridPublicKey, HybridSignature, HybridSigner, HybridVerifier};
use std::time::Instant;

pub fn run(args: VerifyBatchArgs) -> Result<Report, String> {
    if args.items == 0 {
        return Err("--items must be > 0".into());
    }
    if args.iterations == 0 {
        return Err("--iterations must be > 0".into());
    }

    // Default key fan-out: 1 key per item (worst case for the cache,
    // fairest comparison). `--keys 0` is the sentinel for "default".
    let keys = if args.keys == 0 { args.items } else { args.keys };
    let keys = keys.min(args.items);

    tracing::info!(
        target: "rope_loadgen::verify_batch",
        items = args.items,
        keys,
        payload_bytes = args.payload_bytes,
        iterations = args.iterations,
        cold_cache = args.cold_cache,
        "starting verify-batch microbenchmark",
    );

    // ------------------------------------------------------------------
    // Build the corpus once (outside any timing loop): one keypair per
    // distinct key, then assign each item a key via round-robin and
    // sign a deterministic payload.
    // ------------------------------------------------------------------
    let mut rng = ChaCha8Rng::seed_from_u64(args.seed);

    let mut signers: Vec<HybridSigner> = Vec::with_capacity(keys);
    let mut public_keys: Vec<HybridPublicKey> = Vec::with_capacity(keys);
    for _ in 0..keys {
        let mut seed = [0u8; 32];
        rng.fill_bytes(&mut seed);
        let (signer, pk) = HybridSigner::from_seed(&seed);
        signers.push(signer);
        public_keys.push(pk);
    }

    let mut messages: Vec<Vec<u8>> = Vec::with_capacity(args.items);
    let mut signatures: Vec<HybridSignature> = Vec::with_capacity(args.items);
    let mut item_to_key: Vec<usize> = Vec::with_capacity(args.items);
    for i in 0..args.items {
        let key_idx = i % keys;
        item_to_key.push(key_idx);
        let mut payload = vec![0u8; args.payload_bytes];
        rng.fill_bytes(&mut payload);
        // Suffix the payload with the item index so every item has a
        // distinct signing message (avoids accidental dedup).
        if payload.len() >= 8 {
            payload[..8].copy_from_slice(&(i as u64).to_le_bytes());
        }
        signatures.push(signers[key_idx].sign(&payload));
        messages.push(payload);
    }

    // Sanity check: a single warm-up run on the batch path catches
    // any plumbing bug before we start timing.
    {
        let warmup_items: Vec<BatchVerifyItem<'_>> = (0..args.items)
            .map(|i| {
                BatchVerifyItem::new(&public_keys[item_to_key[i]], &messages[i], &signatures[i])
            })
            .collect();
        let outcome = HybridVerifier::verify_batch(&warmup_items)
            .map_err(|e| format!("warmup verify_batch failed: {e}"))?;
        if !outcome.all_valid {
            return Err(format!(
                "warmup verify_batch produced {} invalid item(s) out of {} — corpus is corrupt",
                outcome.results.iter().filter(|&&b| !b).count(),
                outcome.batch_size
            ));
        }
    }

    // ------------------------------------------------------------------
    // Timing loop: alternate batch/serial each iteration to keep
    // CPU/cache state similar across both paths.
    // ------------------------------------------------------------------
    let mut serial_elapsed_ns = 0u128;
    let mut batch_elapsed_ns = 0u128;

    for iter in 0..args.iterations {
        if args.cold_cache {
            HybridVerifier::clear_pq_cache();
        }

        let items: Vec<BatchVerifyItem<'_>> = (0..args.items)
            .map(|i| {
                BatchVerifyItem::new(&public_keys[item_to_key[i]], &messages[i], &signatures[i])
            })
            .collect();

        // Batch path first (so a "cold" iteration measures cold-cache
        // cost on the batch path; the serial path will then benefit
        // from the now-warm cache, slightly biasing AGAINST the batch
        // path — which is the safer error to make).
        let b_start = Instant::now();
        let outcome = HybridVerifier::verify_batch(&items)
            .map_err(|e| format!("verify_batch failed at iter {iter}: {e}"))?;
        let b_elapsed = b_start.elapsed();
        if !outcome.all_valid {
            return Err(format!(
                "verify_batch reported {} invalid item(s) at iter {iter}",
                outcome.results.iter().filter(|&&b| !b).count()
            ));
        }
        batch_elapsed_ns += b_elapsed.as_nanos();

        let s_start = Instant::now();
        let mut all_ok = true;
        for it in &items {
            let ok = HybridVerifier::verify(it.public_key, it.message, it.signature)
                .map_err(|e| format!("serial verify failed at iter {iter}: {e}"))?;
            all_ok &= ok;
        }
        let s_elapsed = s_start.elapsed();
        if !all_ok {
            return Err(format!("serial verify reported invalid item(s) at iter {iter}"));
        }
        serial_elapsed_ns += s_elapsed.as_nanos();

        tracing::debug!(
            target: "rope_loadgen::verify_batch",
            iter,
            serial_us = s_elapsed.as_micros(),
            batch_us = b_elapsed.as_micros(),
            "iteration done",
        );
    }

    let serial_mean_ns = serial_elapsed_ns / args.iterations as u128;
    let batch_mean_ns = batch_elapsed_ns / args.iterations as u128;

    let serial_mean_s = serial_mean_ns as f64 / 1e9;
    let batch_mean_s = batch_mean_ns as f64 / 1e9;

    let serial_throughput = if serial_mean_s > 0.0 {
        args.items as f64 / serial_mean_s
    } else {
        0.0
    };
    let batch_throughput = if batch_mean_s > 0.0 {
        args.items as f64 / batch_mean_s
    } else {
        0.0
    };
    let speedup = if batch_mean_ns > 0 {
        serial_mean_ns as f64 / batch_mean_ns as f64
    } else {
        0.0
    };
    let serial_per_item_us = serial_mean_ns as f64 / 1_000.0 / args.items as f64;
    let batch_per_item_us = batch_mean_ns as f64 / 1_000.0 / args.items as f64;

    let logical_cpus = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    Ok(Report::VerifyBatch(VerifyBatchReport {
        items: args.items,
        keys,
        payload_bytes: args.payload_bytes,
        iterations: args.iterations,
        cold_cache: args.cold_cache,
        seed: args.seed,
        logical_cpus,
        serial_elapsed_ms: serial_mean_ns as f64 / 1_000_000.0,
        batch_elapsed_ms: batch_mean_ns as f64 / 1_000_000.0,
        serial_throughput_ops_per_sec: serial_throughput,
        batch_throughput_ops_per_sec: batch_throughput,
        batch_speedup_x: speedup,
        serial_per_item_us,
        batch_per_item_us,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_batch_runs_to_completion() {
        let args = VerifyBatchArgs {
            items: 8,
            keys: 4,
            payload_bytes: 32,
            iterations: 2,
            cold_cache: false,
            seed: 42,
        };
        let report = run(args).expect("benchmark must succeed");
        match report {
            Report::VerifyBatch(r) => {
                assert_eq!(r.items, 8);
                assert_eq!(r.keys, 4);
                assert_eq!(r.iterations, 2);
                assert!(r.serial_elapsed_ms > 0.0);
                assert!(r.batch_elapsed_ms > 0.0);
                assert!(r.serial_throughput_ops_per_sec > 0.0);
                assert!(r.batch_throughput_ops_per_sec > 0.0);
                // Speedup is allowed to be < 1 on single-core CI; we
                // only check it's a sane positive number.
                assert!(r.batch_speedup_x > 0.0);
            }
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn rejects_zero_items() {
        let args = VerifyBatchArgs {
            items: 0,
            keys: 0,
            payload_bytes: 32,
            iterations: 1,
            cold_cache: false,
            seed: 1,
        };
        assert!(run(args).is_err());
    }

    #[test]
    fn keys_zero_means_one_key_per_item() {
        let args = VerifyBatchArgs {
            items: 4,
            keys: 0,
            payload_bytes: 16,
            iterations: 1,
            cold_cache: false,
            seed: 1,
        };
        let report = run(args).expect("benchmark must succeed");
        match report {
            Report::VerifyBatch(r) => assert_eq!(r.keys, 4),
            _ => panic!("wrong report variant"),
        }
    }

    #[test]
    fn keys_capped_at_items() {
        let args = VerifyBatchArgs {
            items: 4,
            keys: 100, // > items
            payload_bytes: 16,
            iterations: 1,
            cold_cache: false,
            seed: 1,
        };
        let report = run(args).expect("benchmark must succeed");
        match report {
            Report::VerifyBatch(r) => assert_eq!(r.keys, 4),
            _ => panic!("wrong report variant"),
        }
    }
}
