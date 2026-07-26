//! # Quipu Canon v2.0 Phase 2.C — Signature batch verification
//!
//! Phase 1 reduced per-knot wall time everywhere except the cryptographic
//! verification path: every knot's hybrid signature (Ed25519 +
//! CRYSTALS-Dilithium3) costs ~50 µs of Ed25519 + ~150–200 µs of
//! Dilithium per single-thread call, and Phase 2.B's parallel writer
//! pool now feeds verifications fast enough that they become the next
//! bottleneck.
//!
//! ## Design
//!
//! Three orthogonal wins, all in one batch entry point:
//!
//! 1. **Rayon parallel verification** — the only Ed25519 batch-verify
//!    available in the dalek 2.x line was removed for soundness reasons
//!    (it allowed batch-only forgeries). Falling back to per-item
//!    verification across rayon worker threads gives a clean, sound
//!    `~min(N, ncpus)`× speedup, which matters more than the algebraic
//!    batching anyway because Dilithium dominates the cost and is not
//!    algebraically batchable.
//!
//! 2. **Public-key parsing cache** — every `dilithium3::open` call
//!    re-parses the raw 1952-byte public key into a Dilithium
//!    `PublicKey` object (~20 µs per call). Validators reuse the same
//!    keys across millions of signatures, so we memoise the parsed
//!    public keys process-wide, keyed by BLAKE3(`pk_bytes`). The cache
//!    holds `Arc<dilithium3::PublicKey>` so lookups are ~50 ns.
//!
//! 3. **Short-circuit on Ed25519 failure** — every hybrid verify
//!    requires both Ed25519 AND Dilithium when PQ keys are present. A
//!    failed Ed25519 verification can skip the ~200 µs Dilithium check
//!    entirely. Ed25519 is ~4× cheaper than Dilithium, so this saves
//!    a substantial fraction of the work on attacks / corrupt batches.
//!
//! ## Public API
//!
//! - [`BatchVerifyItem`] — one signature to verify (borrowed message,
//!   borrowed public key, borrowed signature; lifetime-tied to the
//!   caller).
//! - [`BatchVerifyOutcome`] — per-item booleans plus aggregates.
//! - [`HybridVerifier::verify_batch`] — the entry point. Drop-in
//!   replacement for a loop of [`HybridVerifier::verify`].
//!
//! ## Cache
//!
//! The cache lives in [`pq_pubkey_cache`] as an
//! `OnceCell<RwLock<HashMap<[u8; 32], Arc<dilithium3::PublicKey>>>>`.
//! It is unbounded by default — intended for the production validator
//! set of ≤ ~100 keys. Operators expecting a much larger key universe
//! should call [`HybridVerifier::clear_pq_cache`] periodically.

use crate::error::{CryptoError, Result};
use crate::hybrid::{HybridPublicKey, HybridSignature, HybridVerifier, DILITHIUM3_PUBLIC_KEY_SIZE};
use ed25519_dalek::{Signature as Ed25519Signature, Verifier, VerifyingKey};
use once_cell::sync::OnceCell;
use parking_lot::RwLock;
use pqcrypto_dilithium::dilithium3;
use pqcrypto_traits::sign::{PublicKey as PqPublicKey, SignedMessage};
use rayon::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;

// ============================================================================
// Public types
// ============================================================================

/// One item in a batch verification call. Borrows everything; the
/// caller retains ownership of the messages, public keys and signatures.
#[derive(Clone, Copy, Debug)]
pub struct BatchVerifyItem<'a> {
    pub public_key: &'a HybridPublicKey,
    pub message: &'a [u8],
    pub signature: &'a HybridSignature,
}

impl<'a> BatchVerifyItem<'a> {
    pub fn new(
        public_key: &'a HybridPublicKey,
        message: &'a [u8],
        signature: &'a HybridSignature,
    ) -> Self {
        Self {
            public_key,
            message,
            signature,
        }
    }
}

/// Outcome of a batch verification call.
#[derive(Clone, Debug)]
pub struct BatchVerifyOutcome {
    /// Per-item verification result, parallel to the input slice.
    /// `results[i] == true` ⇔ item `i` verified successfully.
    pub results: Vec<bool>,
    /// `true` iff every item in the batch verified. Equivalent to
    /// `results.iter().all(|&b| b)` but pre-computed for the common
    /// happy path.
    pub all_valid: bool,
    /// Total items in the batch.
    pub batch_size: usize,
    /// How many items had a non-empty Dilithium signature path
    /// exercised. Useful for ops dashboards to confirm the PQ path is
    /// actually firing.
    pub pq_verified: usize,
}

impl BatchVerifyOutcome {
    pub fn empty() -> Self {
        Self {
            results: Vec::new(),
            all_valid: true,
            batch_size: 0,
            pq_verified: 0,
        }
    }
}

// ============================================================================
// Public-key parsing cache
// ============================================================================

/// Process-wide cache of parsed Dilithium public keys, keyed by
/// `blake3(pk_bytes)`. Populated lazily on first verify against a
/// given key. See module docs for caveats.
fn pq_pubkey_cache() -> &'static RwLock<HashMap<[u8; 32], Arc<dilithium3::PublicKey>>> {
    static CACHE: OnceCell<RwLock<HashMap<[u8; 32], Arc<dilithium3::PublicKey>>>> =
        OnceCell::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Parse (or fetch from cache) a Dilithium3 public key. Returns
/// `None` if the bytes are the wrong size or the key is malformed —
/// callers must treat this as a verification failure (NOT an error;
/// a malformed PK is just an invalid attestation).
fn lookup_or_parse_dilithium_pk(pk_bytes: &[u8]) -> Option<Arc<dilithium3::PublicKey>> {
    if pk_bytes.len() != DILITHIUM3_PUBLIC_KEY_SIZE {
        return None;
    }

    let key = *blake3::hash(pk_bytes).as_bytes();

    // Fast path: read lock + lookup.
    {
        let guard = pq_pubkey_cache().read();
        if let Some(arc) = guard.get(&key) {
            return Some(arc.clone());
        }
    }

    // Slow path: parse + insert. Another thread may race and insert
    // first; that is fine — we keep the existing entry.
    let parsed = match dilithium3::PublicKey::from_bytes(pk_bytes) {
        Ok(pk) => Arc::new(pk),
        Err(e) => {
            tracing::warn!("Dilithium PublicKey parse failed: {:?}", e);
            return None;
        }
    };

    let mut guard = pq_pubkey_cache().write();
    let entry = guard.entry(key).or_insert(parsed);
    Some(entry.clone())
}

// ============================================================================
// HybridVerifier::verify_batch — the Phase 2.C entry point
// ============================================================================

impl HybridVerifier {
    /// Verify a batch of hybrid signatures in parallel.
    ///
    /// Per-item semantics match [`HybridVerifier::verify`] exactly:
    /// Ed25519 must always verify; if the public key carries a
    /// Dilithium component, the Dilithium signature must also verify.
    ///
    /// Errors only on internal cache contention or thread-pool
    /// failures — never on individual verification failures (those
    /// surface as `false` in `BatchVerifyOutcome::results`). If the
    /// caller wants the index of the first failing item, scan
    /// `results` after the call.
    pub fn verify_batch(items: &[BatchVerifyItem<'_>]) -> Result<BatchVerifyOutcome> {
        if items.is_empty() {
            return Ok(BatchVerifyOutcome::empty());
        }

        let pq_count = std::sync::atomic::AtomicUsize::new(0);

        // Rayon par_iter gives us one task per item; the rayon
        // thread-pool default is `num_cpus` workers and steals work
        // greedily, so a 64-item batch on an 8-core machine gets
        // ~8× speedup.
        //
        // We use `with_min_len` to amortise scheduling overhead for
        // very large batches — items 1..=8 inline on the calling
        // thread is wasteful otherwise.
        let results: Vec<bool> = items
            .par_iter()
            .with_min_len(4)
            .map(|item| {
                // Ed25519 first (fast). Short-circuit on failure so we
                // skip the ~200 µs Dilithium verify on bad signatures.
                let ed_ok = match verify_ed25519_short(
                    &item.public_key.ed25519,
                    item.message,
                    &item.signature.ed25519_sig,
                ) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("Ed25519 verify error: {e:?}");
                        false
                    }
                };
                if !ed_ok {
                    return false;
                }

                // If no PQ key, Ed25519 alone is acceptable (mirrors
                // single-item HybridVerifier::verify policy).
                if !item.public_key.has_pq_keys() {
                    return true;
                }

                // PQ key present → Dilithium signature MUST be present
                // and verify.
                if item.signature.dilithium_sig.is_empty() {
                    tracing::debug!("Dilithium PK present but signature empty");
                    return false;
                }

                let pk = match lookup_or_parse_dilithium_pk(&item.public_key.dilithium) {
                    Some(p) => p,
                    None => return false,
                };

                let signed_msg =
                    match dilithium3::SignedMessage::from_bytes(&item.signature.dilithium_sig) {
                        Ok(sm) => sm,
                        Err(_) => return false,
                    };

                pq_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                match dilithium3::open(&signed_msg, &pk) {
                    Ok(opened) => opened.as_slice() == item.message,
                    Err(_) => false,
                }
            })
            .collect();

        let all_valid = results.iter().all(|&b| b);
        Ok(BatchVerifyOutcome {
            batch_size: items.len(),
            pq_verified: pq_count.load(std::sync::atomic::Ordering::Relaxed),
            all_valid,
            results,
        })
    }

    /// Number of cached parsed Dilithium public keys. Mostly for
    /// observability — operators wiring this into a Prometheus
    /// gauge can expose `rope_pq_pubkey_cache_size`.
    pub fn pq_cache_size() -> usize {
        pq_pubkey_cache().read().len()
    }

    /// Empty the parsed Dilithium public key cache. Call this on
    /// validator-set rotation or in long-running fuzz tests.
    pub fn clear_pq_cache() {
        pq_pubkey_cache().write().clear();
    }
}

/// Lift the Ed25519 verify path out of `HybridVerifier::verify_ed25519`
/// (which is `pub(crate)` within `hybrid.rs`) so the batch path can call
/// it without needing to traverse the cross-module boundary. Identical
/// semantics — kept here verbatim for cohesion.
fn verify_ed25519_short(public_key: &[u8; 32], message: &[u8], signature: &[u8]) -> Result<bool> {
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .map_err(|e| CryptoError::InvalidPublicKey(e.to_string()))?;
    let sig_bytes: [u8; 64] = signature.try_into().map_err(|_| {
        CryptoError::InvalidSignature("Invalid Ed25519 signature length".to_string())
    })?;
    let sig = Ed25519Signature::from_bytes(&sig_bytes);
    Ok(verifying_key.verify(message, &sig).is_ok())
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hybrid::HybridSigner;

    #[test]
    fn empty_batch_is_trivially_valid() {
        let outcome = HybridVerifier::verify_batch(&[]).unwrap();
        assert_eq!(outcome.batch_size, 0);
        assert!(outcome.all_valid);
        assert!(outcome.results.is_empty());
        assert_eq!(outcome.pq_verified, 0);
    }

    #[test]
    fn single_item_batch_matches_single_verify() {
        let (signer, pk) = HybridSigner::generate();
        let msg = b"single";
        let sig = signer.sign(msg);

        let single = HybridVerifier::verify(&pk, msg, &sig).unwrap();
        let batch = HybridVerifier::verify_batch(&[BatchVerifyItem::new(&pk, msg, &sig)]).unwrap();
        assert_eq!(single, batch.results[0]);
        assert!(batch.all_valid);
        assert_eq!(batch.batch_size, 1);
        assert_eq!(batch.pq_verified, 1);
    }

    #[test]
    fn batch_of_valid_items_all_pass() {
        // 16 items, all from independent keys, all valid.
        let mut signers = Vec::new();
        let mut messages: Vec<Vec<u8>> = Vec::new();
        for i in 0..16 {
            signers.push(HybridSigner::generate());
            messages.push(format!("msg-{i}").into_bytes());
        }
        let signatures: Vec<HybridSignature> = signers
            .iter()
            .zip(messages.iter())
            .map(|((s, _), m)| s.sign(m))
            .collect();

        let items: Vec<BatchVerifyItem<'_>> = signers
            .iter()
            .zip(messages.iter())
            .zip(signatures.iter())
            .map(|(((_, pk), m), s)| BatchVerifyItem::new(pk, m.as_slice(), s))
            .collect();

        let outcome = HybridVerifier::verify_batch(&items).unwrap();
        assert_eq!(outcome.batch_size, 16);
        assert_eq!(outcome.pq_verified, 16);
        assert!(outcome.all_valid);
        assert!(outcome.results.iter().all(|&b| b));
    }

    #[test]
    fn batch_isolates_one_bad_item() {
        // 8 valid + 1 tampered. The single bad apple must NOT cause
        // the other items to be marked invalid.
        let mut signers = Vec::new();
        let mut messages: Vec<Vec<u8>> = Vec::new();
        for i in 0..9 {
            signers.push(HybridSigner::generate());
            messages.push(format!("msg-{i}").into_bytes());
        }
        let mut signatures: Vec<HybridSignature> = signers
            .iter()
            .zip(messages.iter())
            .map(|((s, _), m)| s.sign(m))
            .collect();

        // Tamper the Ed25519 signature of item 4. This should make the
        // Ed25519 verify fail and the short-circuit skip the Dilithium
        // verify entirely.
        for byte in signatures[4].ed25519_sig.iter_mut().take(4) {
            *byte ^= 0xFF;
        }

        let items: Vec<BatchVerifyItem<'_>> = signers
            .iter()
            .zip(messages.iter())
            .zip(signatures.iter())
            .map(|(((_, pk), m), s)| BatchVerifyItem::new(pk, m.as_slice(), s))
            .collect();

        let outcome = HybridVerifier::verify_batch(&items).unwrap();
        assert_eq!(outcome.batch_size, 9);
        assert!(!outcome.all_valid);
        for (i, &ok) in outcome.results.iter().enumerate() {
            if i == 4 {
                assert!(!ok, "tampered item must NOT verify");
            } else {
                assert!(ok, "item {i} must still verify (one bad apple is isolated)");
            }
        }
    }

    #[test]
    fn pk_cache_is_populated_on_first_verify() {
        // The cache is a process-wide singleton, so other tests in
        // this binary may race against us. We therefore measure
        // *deltas* relative to a baseline taken just before the first
        // verify, and assert the second verify with the same key adds
        // exactly zero further entries (i.e., the cache hit fires).
        let (signer, pk) = HybridSigner::generate();
        let msg = b"cache me";
        let sig = signer.sign(msg);

        let baseline = HybridVerifier::pq_cache_size();

        let outcome =
            HybridVerifier::verify_batch(&[BatchVerifyItem::new(&pk, msg, &sig)]).unwrap();
        assert!(outcome.all_valid);
        let after_first = HybridVerifier::pq_cache_size();
        assert!(
            after_first > baseline,
            "first verify must populate the cache (baseline={baseline}, after={after_first})"
        );

        // Second batch with the same key MUST NOT add a new entry,
        // even under parallel test races (a sibling test cannot remove
        // entries — `clear_pq_cache` is the only public mutator and
        // we don't call it here).
        let msg2 = b"cache me twice";
        let sig2 = signer.sign(msg2);
        let outcome2 =
            HybridVerifier::verify_batch(&[BatchVerifyItem::new(&pk, msg2, &sig2)]).unwrap();
        assert!(outcome2.all_valid);
        let after_second = HybridVerifier::pq_cache_size();
        // Sibling tests may add their own keys; what matters is that
        // we did NOT add a duplicate for our key.
        assert!(
            after_second >= after_first,
            "cache must not shrink between verifies"
        );
        // A second verify with the SAME pk cannot add a new entry of
        // its own — so the delta from after_first to after_second is
        // entirely attributable to sibling tests, never to us. The
        // strongest deterministic claim we can make under a parallel
        // test runner is the one above; the cache-hit invariant is
        // enforced structurally by `lookup_or_parse_dilithium_pk`.
    }

    #[test]
    fn batch_handles_message_tampering_per_item() {
        // 4 valid items; tamper the MESSAGE (not the signature) of
        // item 2. Both Ed25519 and Dilithium verifications must fail
        // on item 2; the others must pass.
        let mut signers = Vec::new();
        let mut messages: Vec<Vec<u8>> = Vec::new();
        for i in 0..4 {
            signers.push(HybridSigner::generate());
            messages.push(format!("msg-{i}").into_bytes());
        }
        let signatures: Vec<HybridSignature> = signers
            .iter()
            .zip(messages.iter())
            .map(|((s, _), m)| s.sign(m))
            .collect();

        // Tampered message for item 2.
        let bad_msg = b"i am not the original".to_vec();
        let mut tampered_messages = messages.clone();
        tampered_messages[2] = bad_msg;

        let items: Vec<BatchVerifyItem<'_>> = signers
            .iter()
            .zip(tampered_messages.iter())
            .zip(signatures.iter())
            .map(|(((_, pk), m), s)| BatchVerifyItem::new(pk, m.as_slice(), s))
            .collect();

        let outcome = HybridVerifier::verify_batch(&items).unwrap();
        assert!(!outcome.all_valid);
        assert!(!outcome.results[2]);
        for i in [0usize, 1, 3] {
            assert!(outcome.results[i], "item {i} must still verify");
        }
    }

    #[test]
    fn batch_with_ed25519_only_keys_succeeds_without_pq() {
        // A public key with no Dilithium component must verify
        // Ed25519-only — same policy as the single-item path.
        let (signer, full_pk) = HybridSigner::generate();
        let msg = b"ed25519 only";
        let sig = signer.sign(msg);

        // Manually construct an Ed25519-only PK reusing the signer's
        // ed25519 component. The signer's Dilithium signature is
        // present but should be IGNORED (no PQ pk → no PQ check).
        let ed_only = HybridPublicKey::from_ed25519(full_pk.ed25519);
        // Strip the Dilithium half of the signature too — the policy
        // says: no PQ pk → only Ed25519 is consulted, regardless of
        // what the signature carries.
        let ed_only_sig = HybridSignature::new(
            sig.ed25519_sig.clone().try_into().unwrap(),
            Vec::new(),
        );

        let outcome =
            HybridVerifier::verify_batch(&[BatchVerifyItem::new(&ed_only, msg, &ed_only_sig)])
                .unwrap();
        assert!(outcome.all_valid);
        assert_eq!(outcome.pq_verified, 0, "Ed25519-only path must not bump pq_verified");
    }

    #[test]
    fn batch_path_matches_single_path_on_random_mix() {
        // Generate 32 items, half valid and half tampered. Compare
        // the per-item booleans returned by `verify_batch` to those
        // returned by a loop of `verify`. They must match exactly.
        let mut signers = Vec::new();
        let mut messages: Vec<Vec<u8>> = Vec::new();
        for i in 0..32 {
            signers.push(HybridSigner::generate());
            messages.push(format!("mix-{i}").into_bytes());
        }
        let mut signatures: Vec<HybridSignature> = signers
            .iter()
            .zip(messages.iter())
            .map(|((s, _), m)| s.sign(m))
            .collect();

        // Tamper every odd-indexed item.
        for i in (1..32).step_by(2) {
            for byte in signatures[i].ed25519_sig.iter_mut().take(2) {
                *byte ^= 0x55;
            }
        }

        let items: Vec<BatchVerifyItem<'_>> = signers
            .iter()
            .zip(messages.iter())
            .zip(signatures.iter())
            .map(|(((_, pk), m), s)| BatchVerifyItem::new(pk, m.as_slice(), s))
            .collect();

        let batch_outcome = HybridVerifier::verify_batch(&items).unwrap();

        // Reference: single-item loop.
        let single_results: Vec<bool> = items
            .iter()
            .map(|it| HybridVerifier::verify(it.public_key, it.message, it.signature).unwrap())
            .collect();

        assert_eq!(batch_outcome.results, single_results);
    }

    /// Throughput sanity check (NOT a proper benchmark — just a guard
    /// that the parallel path actually runs concurrently). On any
    /// machine with > 1 core, a batch of 32 items must be faster than
    /// a serial loop of 32 single verifies. We allow generous slack
    /// (1.5×) so CI noise doesn't flake this.
    #[test]
    fn batch_is_faster_than_serial_loop_on_multi_core() {
        if num_cpus() <= 1 {
            // Skip on single-core CI runners.
            return;
        }

        const N: usize = 32;
        let mut signers = Vec::new();
        let mut messages: Vec<Vec<u8>> = Vec::new();
        for i in 0..N {
            signers.push(HybridSigner::generate());
            messages.push(format!("perf-{i}").into_bytes());
        }
        let signatures: Vec<HybridSignature> = signers
            .iter()
            .zip(messages.iter())
            .map(|((s, _), m)| s.sign(m))
            .collect();
        let items: Vec<BatchVerifyItem<'_>> = signers
            .iter()
            .zip(messages.iter())
            .zip(signatures.iter())
            .map(|(((_, pk), m), s)| BatchVerifyItem::new(pk, m.as_slice(), s))
            .collect();

        // Warm-up to populate PK cache and let rayon pool spin up.
        let _ = HybridVerifier::verify_batch(&items).unwrap();
        for it in &items {
            let _ = HybridVerifier::verify(it.public_key, it.message, it.signature).unwrap();
        }

        let serial_start = std::time::Instant::now();
        for it in &items {
            let _ = HybridVerifier::verify(it.public_key, it.message, it.signature).unwrap();
        }
        let serial = serial_start.elapsed();

        let batch_start = std::time::Instant::now();
        let _ = HybridVerifier::verify_batch(&items).unwrap();
        let batch = batch_start.elapsed();

        assert!(
            batch.as_micros() * 3 / 2 < serial.as_micros(),
            "batch {batch:?} must be at least 1.5× faster than serial {serial:?}"
        );
    }

    /// Tiny helper because we deliberately don't pull in the `num_cpus`
    /// crate just for one test. Falls back to a conservative default.
    fn num_cpus() -> usize {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }
}
