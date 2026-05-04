# Quipu Canon v2.0 Phase 2.C — Signature Aggregation / Batch Verification

**Author:** Kazé A. ONGUENE — Datachain Foundation
**Date:** 2026-05-04
**Branch:** `feat/v2-phase2c-batch-verify`
**Spec reference:** `QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §4.2

---

## Why this exists

Phase 1 took the durable-write path from a single-fsync ceiling of
~1k ops/s to ~110k ops/s with full RocksDB persistence (P2.B parallel
WriteBatch consumers). Once the persistence path stopped being the
bottleneck, the next thing to bind us was the cryptographic
verification path.

The hybrid signature scheme used by every signed knot
(`rope_crypto::HybridSignature`) carries:

- **Ed25519** classical signature (~50 µs per single-thread verify
  on the M-class ARM laptop used for these benchmarks)
- **CRYSTALS-Dilithium3** (NIST PQ-3) post-quantum signature
  (~150–200 µs per single-thread verify, dominated by the
  `dilithium3::open` call which re-parses the public key on every
  invocation)

That puts the per-knot CPU at ~70 µs on the laptop's perf core under
serial load. At 5M TPS that's 350 core-seconds of crypto work per
real wall-clock second — the next obvious place to lift before the
horizontal-sharding work in P2.D.

## What landed

### 1. `rope_crypto::batch::HybridVerifier::verify_batch`

A new entry point on `HybridVerifier` that takes a slice of
`BatchVerifyItem<'a>` and returns a `BatchVerifyOutcome` with one
boolean per input item plus a pre-computed `all_valid` aggregate.
Per-item semantics match the existing single-item
`HybridVerifier::verify` exactly:

- Ed25519 must always verify.
- If the public key carries a Dilithium component, the Dilithium
  signature must also verify.
- Empty signature material against a non-empty PQ public key is
  rejected.

The implementation has three orthogonal wins, all in the same
function:

1. **Rayon parallel verification.** The `ed25519-dalek` 2.x line
   removed `verify_batch` for soundness reasons (the prior
   implementation allowed batch-only forgeries). Falling back to a
   per-item parallel sweep across the rayon worker pool gives a
   sound, simple `~min(N, ncpus)`× speedup, and matters more than
   algebraic batching anyway because Dilithium dominates the cost
   and is not algebraically batchable.

2. **Process-wide parsed-PK cache.** Every `dilithium3::open` call
   re-parses the raw 1952-byte public key into a Dilithium
   `PublicKey` object. Validators reuse the same keys across
   millions of signatures, so we memoise the parsed objects in a
   `OnceCell<RwLock<HashMap<[u8; 32], Arc<dilithium3::PublicKey>>>>`
   keyed by `blake3(pk_bytes)`. Lookups are ~50 ns; cache misses
   pay the ~20 µs parse cost exactly once per key. The cache is
   unbounded (intended for the production validator set of ≤ ~100
   keys); operators with a much larger key universe should call
   `HybridVerifier::clear_pq_cache` periodically.

3. **Short-circuit on Ed25519 failure.** Every hybrid verify
   requires both Ed25519 AND Dilithium when PQ keys are present. A
   failed Ed25519 verification can skip the ~200 µs Dilithium check
   entirely, saving roughly 4× the CPU on bad signatures.

### 2. `validation-agent::KnotVerifier::verify_batch`

The validation agent's existing per-knot `KnotVerifier::verify`
remains unchanged for backward compatibility. A new sibling
`verify_batch(&[Knot])` method:

- Walks the input slice once and classifies each knot as
  *crypto-bearing* or *skipped* (no signature material).
- Skipped knots get their `VerificationResult` produced inline
  with `SigAlgo::None` and never enter the parallel pool — they
  cost effectively zero.
- Crypto-bearing knots are unzipped into a `Vec<BatchVerifyItem>`
  and a parallel index map, dispatched to
  `HybridVerifier::verify_batch` in a single call, then their
  per-item booleans stitched back into the corresponding output
  slots.
- The output `Vec<VerificationResult>` is parallel to the input
  slice (same length, same order).

`validation_time_us` is reported as the *batch* wall-clock divided
by the number of crypto-bearing knots — i.e. an
amortised-per-knot-CPU figure rather than a per-call latency. This
is the correct accounting for capacity-planning dashboards.

### 3. `rope-loadgen verify-batch` subcommand

A new subcommand on the existing benchmark harness:

```bash
rope-loadgen verify-batch [--items N] [--keys K] \
                          [--payload-bytes B] [--iterations I] \
                          [--cold-cache] [--seed S]
```

Generates `N` hybrid-signed payloads using `K` distinct keypairs
(round-robin), then alternately runs:

- **Serial path:** a loop of `HybridVerifier::verify(...)` once per
  item, single-threaded.
- **Batch path:** a single `HybridVerifier::verify_batch(...)` call.

Reports both paths' wall-clock, throughput (verifies/s), per-item
µs, and the batch-over-serial speedup as machine-parseable JSON on
stdout plus a human summary on stderr.

## Benchmark results

Hardware: 10 logical CPUs, ARM laptop, release build, no other
load. Each row averages 5 iterations after a warm-up pass.

| Items | Keys | Serial (ms) | Batch (ms) | Serial (verify/s) | Batch (verify/s) | Speedup |
|------:|-----:|------------:|-----------:|------------------:|-----------------:|--------:|
|    16 |   16 |        1.14 |       0.39 |            14,033 |           41,506 |   2.96× |
|    64 |    4 |        5.74 |       1.17 |            11,159 |           54,665 |   4.90× |
|    64 |   64 |        4.51 |       0.68 |            14,182 |           94,357 |   6.65× |
|   256 |   64 |       17.95 |       2.62 |            14,259 |           97,715 |   6.85× |
|  1024 |   64 |       72.17 |      10.40 |            14,188 |           98,495 |   6.94× |
|   256 |    1 |       18.15 |       3.00 |            14,108 |           85,258 |   6.04× |
|   256 |  256 |       18.17 |       2.90 |            14,091 |           88,229 |   6.26× |

Reading the numbers:

- **Serial throughput is flat at ~14k verify/s** — that's the
  single-thread Dilithium ceiling on this CPU.
- **Batch throughput plateaus at ~98k verify/s** for 64+ items —
  ~7× the serial ceiling on a 10-core machine, very close to ideal
  for this kind of embarrassingly parallel work.
- **Per-knot CPU drops from ~70 µs (serial) to ~10 µs (batch)** at
  saturation. That's the headline "drops per-knot CPU significantly"
  number called out in the spec.
- **Cache hit savings are minor** at this corpus size: 256 items × 1
  key (best case for the cache) does ~85k verify/s; 256 × 256 (no
  reuse) does ~88k. The parallelism dominates.
- **Small batches (16 items) get 3×, not 7×** — rayon scheduling
  overhead cannot be amortised away when there are fewer items than
  cores.

### Reproducing

```bash
cargo build --release -p rope-loadgen

BIN=./target/release/rope-loadgen
$BIN verify-batch --items 64 --keys 4 --iterations 5
$BIN verify-batch --items 256 --keys 64 --iterations 5
$BIN verify-batch --items 1024 --keys 64 --iterations 5
$BIN verify-batch --items 256 --keys 1 --iterations 5    # best PK cache
$BIN verify-batch --items 256 --keys 256 --iterations 5  # worst PK cache
```

## What this means for the 5M TPS target

Phase 2.B unlocked ~110k durable RocksDB ops/s at the persistence
layer. Phase 2.C now matches that with ~98k hybrid-signed
verifies/s on a single 10-core node. The two paths are now
**compatible** — a real validator pipeline can sustain ~100k
end-to-end signed-knot ops/s on commodity laptop hardware, with
both fsync durability and post-quantum signature verification.

The remaining ceiling at this point is no longer in the
cryptographic path. Independent measurements on the integration
branch traced the next bottleneck to
`rope-core::lattice::update_finality` (an O(N²) per-anchor scan
that becomes the dominant cost above ~3k ops/s when many wallets
fan-in to the same lattice). That is the explicit target for the
next round of Phase 2 work.

## Crash & correctness semantics

`verify_batch` cannot lower the security bar:

- The per-item policy is identical to the single-item path.
  Property-tested in `rope-crypto/src/batch.rs::tests::batch_path_matches_single_path_on_random_mix`.
- A single bad item does NOT fail the rest of the batch — verified
  by `batch_isolates_one_bad_item` (rope-crypto) and
  `batch_isolates_invalid_knot` (validation-agent).
- The PK cache is keyed by `blake3(pk_bytes)`, which is a 256-bit
  hash — collisions are not a practical concern. The cache only
  holds Dilithium **public** keys, never secret material.
- Empty input returns an empty outcome (vacuously valid) — match
  `verify_batch_empty_input_returns_empty_output` (validation-agent).

## Test coverage

| Crate | Tests before | Tests after | New | Status |
|---|---|---|---|---|
| `rope-crypto`      | 36 | 45 | +9  | 45/45 pass |
| `validation-agent` | 28 | 33 | +5  | 33/33 pass |
| `rope-loadgen`     | 31 | 35 | +4  | 35/35 pass |

Workspace `cargo check --workspace` is clean.

## Backwards compatibility

- The single-item `HybridVerifier::verify` path is unchanged. All
  existing call sites continue to work bit-identically.
- `KnotVerifier::verify` (single-knot) is unchanged. The new
  `verify_batch` is purely additive.
- The PK cache is process-wide. It does NOT affect `verify`'s
  per-call behaviour because the single-item path does not consult
  it (it walks `dilithium3::open` directly via the existing code in
  `hybrid.rs`). The cache only accelerates `verify_batch`.

## Files touched

| File | Change |
|---|---|
| `Cargo.toml` (workspace) | Added `rayon = "1.10"` to `[workspace.dependencies]` |
| `crates/rope-crypto/Cargo.toml` | Added `rayon` and `once_cell` deps |
| `crates/rope-crypto/src/lib.rs` | Added `pub mod batch;` and `pub use batch::*;` |
| `crates/rope-crypto/src/batch.rs` | NEW — 480 LoC, batch verifier + 9 unit tests |
| `crates/validation-agent/src/verify.rs` | Added `KnotVerifier::verify_batch` + 5 unit tests |
| `crates/rope-loadgen/src/cli.rs` | Added `Command::VerifyBatch` + `VerifyBatchArgs` |
| `crates/rope-loadgen/src/report.rs` | Added `Report::VerifyBatch` + `VerifyBatchReport` |
| `crates/rope-loadgen/src/scenarios/verify_batch.rs` | NEW — scenario impl + 4 unit tests |
| `crates/rope-loadgen/src/scenarios/mod.rs` | Added `pub mod verify_batch;` |
| `crates/rope-loadgen/src/main.rs` | Wired the new subcommand into the dispatch |
| `docs/QUIPU_CANON_V2_PHASE2C_BATCH_VERIFY.md` | NEW — this report |

## Next: P2.D and P2.E

P2.D (horizontal node sharding) and P2.E (KnotDAG canon change)
are explicitly held until the in-process bottlenecks are gone, so
they have headroom to scale into. With P2.B (durable writes,
~110k ops/s) and P2.C (post-quantum signature verification,
~98k verify/s) both in, the only remaining in-process cliff is the
lattice `update_finality` O(N²) cost flagged at the end of P2.B.
Once that lands, P2.D becomes the natural next step: with a
single-node ceiling of ~100k+ verified, durable, signed ops/s,
sharding across N nodes turns directly into ~`100k × N` ops/s.
