//! # OES Ledger Key Cache — Quipu Canon v2.0 Phase 1.4
//!
//! Memoises `derive_ledger_key((wallet, generation)) -> LedgerKey` so that the
//! 100–199 BLAKE3-round OES `derive_key` work (`crates/rope-crypto/src/oes.rs`,
//! lines 766-801) is paid **once per wallet per OES generation**, not once per
//! knot.
//!
//! ## Why this matters
//!
//! Today, every call to `LedgerManager::append_to_ledger` invokes
//! `rope_crypto::ledger_encryption::derive_ledger_key`, which performs a
//! purpose-string-bound OES derivation. Internally that runs ~30–50µs of
//! iterated BLAKE3 on a ~992 byte genome buffer per call. With wallets emitting
//! many knots per generation (an OES generation rotates every
//! `OES_EVOLUTION_INTERVAL = 100` anchors ≈ several minutes), the derived
//! key is constant for the duration. Caching by `(wallet, generation)` turns
//! a per-knot CPU cost into a per-wallet-per-generation cost — essentially
//! free in steady state.
//!
//! Per the v2.0 architecture spec
//! (`docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §3.4) this is the
//! single biggest CPU win in Phase 1, dropping per-knot CPU from ~50µs to
//! ~10µs (just the AEAD encrypt + BLAKE3 nonce derive).
//!
//! ## Correctness
//!
//! The cache key `(wallet, generation)` is sufficient because
//! `derive_ledger_key` is a pure function of `(OES state at that generation,
//! wallet, generation)`. Within a generation the OES state mutates only via
//! `evolve()` (which advances the generation), so an entry tagged with
//! `generation = G` is valid as long as the OES state at generation `G` was
//! the one used to derive it. The cache only stores entries for the
//! generation each derivation observed; later lookups for `(wallet, G+1)`
//! deterministically miss and re-derive against the new state.
//!
//! Old entries are pruned when the cache exceeds its soft cap, prefering to
//! evict generations strictly below `current_gen - GENERATION_WINDOW`
//! (where `GENERATION_WINDOW = 10`, mirroring `OESManager::is_valid_generation`):
//! those entries can never satisfy a future valid lookup.
//!
//! ## Thread-safety
//!
//! A single `parking_lot::RwLock<HashMap<...>>` guards the table. The hot path
//! is read-only after warmup (cache hit ratio >99% in steady state), so the
//! read lock is uncontended. The write lock is taken only on miss-then-insert
//! and on bulk pruning, both rare events.
//!
//! If profiling in Phase 1 testing shows the lock becomes a bottleneck under
//! extreme parallel insert load (e.g. cold-start with thousands of new
//! wallets), swap to a sharded structure such as `dashmap::DashMap` — the
//! public API of this module is intentionally narrow to keep that swap a
//! single-file change.

use parking_lot::RwLock;
use rope_crypto::ledger_encryption::{derive_ledger_key, LedgerKey, WalletAddress};
use rope_crypto::oes::OESManager;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Default soft cap on cached entries. With ~10K active wallets each holding
/// 1–2 valid generations, ~30K entries is the realistic upper bound; 100K
/// gives a 3× safety margin. At ~100 bytes/entry the cache footprint stays
/// under ~10 MiB.
pub const DEFAULT_MAX_ENTRIES: usize = 100_000;

/// Mirrors `rope_crypto::types::GENERATION_WINDOW` (10). Entries with
/// `generation < current - GENERATION_WINDOW` cannot satisfy any valid
/// future lookup and are first to be evicted under pressure.
pub const PRUNE_BELOW_OFFSET: u64 = 10;

/// Memoised `(wallet, generation) -> LedgerKey` table.
///
/// Wraps an `Arc<LedgerKey>` so concurrent readers can hold the value
/// independent of the cache lock and the underlying key bytes are zeroized
/// when the last `Arc` is dropped (per `LedgerKey`'s `ZeroizeOnDrop`).
pub struct OESKeyCache {
    inner: RwLock<HashMap<(WalletAddress, u64), Arc<LedgerKey>>>,
    max_entries: usize,
    hits: AtomicU64,
    misses: AtomicU64,
    inserts: AtomicU64,
    evictions: AtomicU64,
}

/// Snapshot of cache performance counters. Cheap to compute (atomic loads
/// only); safe to call from a metrics endpoint on the hot path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CacheStats {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub inserts: u64,
    pub evictions: u64,
}

impl CacheStats {
    /// Cache hit ratio in [0.0, 1.0]. Returns 0.0 if no lookups have occurred.
    pub fn hit_ratio(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

impl Default for OESKeyCache {
    fn default() -> Self {
        Self::with_capacity(DEFAULT_MAX_ENTRIES)
    }
}

impl OESKeyCache {
    /// Build a cache with a custom soft cap on entries.
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            max_entries,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            inserts: AtomicU64::new(0),
            evictions: AtomicU64::new(0),
        }
    }

    /// Generic get-or-insert. On miss, invokes `compute` to produce the
    /// key (computed off the cache lock to keep the read fast path
    /// uncontended), then inserts under the write lock.
    ///
    /// Two callers racing on the same `(wallet, generation)` may each call
    /// `compute` once; the second insert wins idempotently because the
    /// underlying derivation is deterministic. We accept this rare double
    /// derivation in exchange for not holding the cache lock across the
    /// (potentially slow) computation.
    pub fn get_or_compute<F>(
        &self,
        wallet: &WalletAddress,
        generation: u64,
        compute: F,
    ) -> Arc<LedgerKey>
    where
        F: FnOnce() -> LedgerKey,
    {
        let key = (wallet.clone(), generation);

        if let Some(hit) = self.inner.read().get(&key).cloned() {
            self.hits.fetch_add(1, Ordering::Relaxed);
            return hit;
        }

        self.misses.fetch_add(1, Ordering::Relaxed);

        let derived = Arc::new(compute());
        self.insert(key, derived.clone());
        derived
    }

    /// Convenience wrapper that wires `OESManager::derive_key` for the
    /// caller. This is the path used by `LedgerManager::append_to_ledger`
    /// and the repatriation read path.
    pub fn get_or_derive_for_oes(
        &self,
        wallet: &WalletAddress,
        generation: u64,
        oes: &OESManager,
    ) -> Arc<LedgerKey> {
        self.get_or_compute(wallet, generation, || {
            derive_ledger_key(
                &|len, purpose| oes.derive_key(len, purpose),
                wallet,
                generation,
            )
        })
    }

    fn insert(&self, key: (WalletAddress, u64), value: Arc<LedgerKey>) {
        let mut guard = self.inner.write();
        if guard.len() >= self.max_entries {
            self.prune_locked(&mut guard, key.1);
        }
        if guard.insert(key, value).is_none() {
            self.inserts.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Evict entries whose generation is strictly below
    /// `current_generation - PRUNE_BELOW_OFFSET`. These can never satisfy
    /// a valid future lookup (mirrors `OESManager::is_valid_generation`).
    ///
    /// Useful to call after each OES `evolve()` to bound memory.
    pub fn prune_below(&self, current_generation: u64) -> usize {
        let mut guard = self.inner.write();
        self.prune_locked(&mut guard, current_generation)
    }

    fn prune_locked(
        &self,
        guard: &mut HashMap<(WalletAddress, u64), Arc<LedgerKey>>,
        current_generation: u64,
    ) -> usize {
        let cutoff = current_generation.saturating_sub(PRUNE_BELOW_OFFSET);
        let before = guard.len();
        guard.retain(|(_, gen), _| *gen >= cutoff);
        let removed = before - guard.len();
        if removed > 0 {
            self.evictions.fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    /// Drop every cached entry for one wallet. Called when a ledger is
    /// erased (`rope_eraseLedger`) so leftover keys don't outlive the
    /// data they encrypted. The `LedgerKey` bytes are zeroized when the
    /// last `Arc` is dropped.
    pub fn invalidate_wallet(&self, wallet: &WalletAddress) -> usize {
        let mut guard = self.inner.write();
        let before = guard.len();
        guard.retain(|(w, _), _| w != wallet);
        let removed = before - guard.len();
        if removed > 0 {
            self.evictions.fetch_add(removed as u64, Ordering::Relaxed);
        }
        removed
    }

    /// Empty the cache. Intended for tests and graceful node shutdown.
    pub fn clear(&self) {
        let mut guard = self.inner.write();
        let n = guard.len();
        guard.clear();
        if n > 0 {
            self.evictions.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    /// Atomic snapshot of cache performance counters.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            entries: self.inner.read().len(),
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            inserts: self.inserts.load(Ordering::Relaxed),
            evictions: self.evictions.load(Ordering::Relaxed),
        }
    }

    /// Soft cap configured at construction.
    pub fn capacity(&self) -> usize {
        self.max_entries
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::thread;

    fn wallet(byte: u8) -> WalletAddress {
        WalletAddress::from_bytes(&[byte; 20])
    }

    /// A test-only stand-in for `OESManager::derive_key`. Counts how many
    /// times the underlying derivation was invoked so we can assert cache
    /// hit/miss accounting.
    struct CountingDeriver {
        calls: AtomicUsize,
    }

    impl CountingDeriver {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn derive(&self, wallet: &WalletAddress, generation: u64) -> LedgerKey {
            self.calls.fetch_add(1, Ordering::Relaxed);
            // Deterministic but cheap: hash (wallet || generation) into 32 bytes.
            let mut input = Vec::with_capacity(wallet.as_bytes().len() + 8);
            input.extend_from_slice(wallet.as_bytes());
            input.extend_from_slice(&generation.to_le_bytes());
            let hash = blake3::hash(&input);
            // Manual construction matches the real `derive_ledger_key`
            // which copies 32 bytes into the LedgerKey.
            let raw = hash.as_bytes().to_vec();
            // We can't construct LedgerKey directly (private field) so we
            // route through derive_ledger_key with a closure that returns
            // exactly our deterministic bytes.
            derive_ledger_key(
                &|len, _purpose| {
                    let mut out = raw.clone();
                    out.truncate(len);
                    out
                },
                wallet,
                generation,
            )
        }
    }

    fn cached(
        cache: &OESKeyCache,
        deriver: &CountingDeriver,
        wallet: &WalletAddress,
        generation: u64,
    ) -> Arc<LedgerKey> {
        cache.get_or_compute(wallet, generation, || deriver.derive(wallet, generation))
    }

    #[test]
    fn miss_then_hit_does_not_re_derive() {
        let cache = OESKeyCache::default();
        let deriver = CountingDeriver::new();
        let w = wallet(1);

        let k1 = cached(&cache, &deriver, &w, 5);
        let k2 = cached(&cache, &deriver, &w, 5);

        assert_eq!(k1.as_bytes(), k2.as_bytes());
        assert_eq!(deriver.calls.load(Ordering::Relaxed), 1);

        let stats = cache.stats();
        assert_eq!(stats.entries, 1);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.inserts, 1);
        assert_eq!(stats.evictions, 0);
    }

    #[test]
    fn distinct_wallets_get_distinct_entries() {
        let cache = OESKeyCache::default();
        let deriver = CountingDeriver::new();

        let _ = cached(&cache, &deriver, &wallet(1), 7);
        let _ = cached(&cache, &deriver, &wallet(2), 7);
        let _ = cached(&cache, &deriver, &wallet(1), 7);

        assert_eq!(deriver.calls.load(Ordering::Relaxed), 2);
        assert_eq!(cache.stats().entries, 2);
    }

    #[test]
    fn distinct_generations_get_distinct_entries() {
        let cache = OESKeyCache::default();
        let deriver = CountingDeriver::new();
        let w = wallet(3);

        let k7 = cached(&cache, &deriver, &w, 7);
        let k8 = cached(&cache, &deriver, &w, 8);

        assert_ne!(
            k7.as_bytes(),
            k8.as_bytes(),
            "OES key derivation must be generation-bound"
        );
        assert_eq!(deriver.calls.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn invalidate_wallet_drops_only_that_wallet() {
        let cache = OESKeyCache::default();
        let deriver = CountingDeriver::new();
        let _ = cached(&cache, &deriver, &wallet(1), 5);
        let _ = cached(&cache, &deriver, &wallet(2), 5);
        let _ = cached(&cache, &deriver, &wallet(1), 6);

        let removed = cache.invalidate_wallet(&wallet(1));
        assert_eq!(removed, 2);
        assert_eq!(cache.stats().entries, 1);
    }

    #[test]
    fn prune_below_drops_stale_generations() {
        let cache = OESKeyCache::default();
        let deriver = CountingDeriver::new();
        for gen in 0..15u64 {
            let _ = cached(&cache, &deriver, &wallet(7), gen);
        }
        assert_eq!(cache.stats().entries, 15);

        let removed = cache.prune_below(20);
        assert_eq!(removed, 10);
        assert_eq!(cache.stats().entries, 5);
        for entry in cache.inner.read().keys() {
            assert!(
                entry.1 >= 10,
                "generation {} should have been pruned",
                entry.1
            );
        }
    }

    #[test]
    fn capacity_pressure_triggers_pruning_on_insert() {
        let cache = OESKeyCache::with_capacity(8);
        let deriver = CountingDeriver::new();

        for gen in 0..6u64 {
            let _ = cached(&cache, &deriver, &wallet(0), gen);
        }
        assert_eq!(cache.stats().entries, 6);

        for gen in 100..104u64 {
            let _ = cached(&cache, &deriver, &wallet(0), gen);
        }

        let stats = cache.stats();
        assert!(
            stats.entries <= 8,
            "cache must respect capacity, got {} entries",
            stats.entries
        );
        assert!(stats.evictions > 0, "stale gens should have been evicted");
    }

    #[test]
    fn parallel_lookups_are_consistent_and_largely_hits() {
        let cache = Arc::new(OESKeyCache::default());
        let deriver = Arc::new(CountingDeriver::new());

        let _ = cached(&cache, &deriver, &wallet(42), 9);

        let mut handles = Vec::new();
        for _ in 0..16 {
            let cache = cache.clone();
            let deriver = deriver.clone();
            handles.push(thread::spawn(move || {
                for _ in 0..200 {
                    let _ = cached(&cache, &deriver, &wallet(42), 9);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        let calls = deriver.calls.load(Ordering::Relaxed);
        assert_eq!(
            calls, 1,
            "parallel hits on a warm key must not re-derive (got {} derivations)",
            calls
        );

        let stats = cache.stats();
        assert_eq!(stats.entries, 1);
        assert!(stats.hits >= 16 * 200);
    }

    #[test]
    fn clear_zeroes_table_and_increments_evictions() {
        let cache = OESKeyCache::default();
        let deriver = CountingDeriver::new();
        for gen in 0..5u64 {
            let _ = cached(&cache, &deriver, &wallet(1), gen);
        }
        assert_eq!(cache.stats().entries, 5);

        cache.clear();
        let stats = cache.stats();
        assert_eq!(stats.entries, 0);
        assert!(stats.evictions >= 5);
    }

    #[test]
    fn stats_hit_ratio_is_well_defined_when_empty() {
        let stats = CacheStats::default();
        assert_eq!(stats.hit_ratio(), 0.0);
    }

    #[test]
    fn stats_hit_ratio_matches_counts() {
        let stats = CacheStats {
            entries: 1,
            hits: 9,
            misses: 1,
            inserts: 1,
            evictions: 0,
        };
        assert!((stats.hit_ratio() - 0.9).abs() < 1e-9);
    }

    /// End-to-end against the real `OESManager` and the production
    /// `get_or_derive_for_oes` entry point. Asserts that the cached
    /// key bytes equal those a fresh `derive_ledger_key` call would
    /// produce, and that a second lookup is a hit.
    #[test]
    fn integration_with_real_oes_manager_produces_correct_key() {
        let oes = OESManager::genesis(&[7u8; 32]);
        let cache = OESKeyCache::default();
        let w = wallet(0xab);
        let gen = oes.generation();

        let cached = cache.get_or_derive_for_oes(&w, gen, &oes);
        let direct = derive_ledger_key(&|len, purpose| oes.derive_key(len, purpose), &w, gen);
        assert_eq!(
            cached.as_bytes(),
            direct.as_bytes(),
            "cached OES key must equal a direct derivation"
        );

        // Second lookup is a hit and returns the same Arc'd key.
        let cached2 = cache.get_or_derive_for_oes(&w, gen, &oes);
        assert!(Arc::ptr_eq(&cached, &cached2));
        assert_eq!(cache.stats().hits, 1);
        assert_eq!(cache.stats().misses, 1);
        assert_eq!(cache.stats().entries, 1);
    }
}
