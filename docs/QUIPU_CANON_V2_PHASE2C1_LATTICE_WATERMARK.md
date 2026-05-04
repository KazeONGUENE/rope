# Quipu Canon v2.0 Phase 2.C.1 — Lattice Finality Watermark

**Author:** Kazé A. ONGUENE — Datachain Foundation
**Date:** 2026-05-04
**Branch:** `feat/v2-phase2c1-lattice-watermark`
**Followup to:** `QUIPU_CANON_V2_PHASE2B_PARALLEL_WRITEBATCH.md`,
`QUIPU_CANON_V2_PHASE2C_BATCH_VERIFY.md`

---

## Why this exists

P2.B's parallel WriteBatch consumers lifted the durable-write
ceiling from ~1k ops/s to ~110k ops/s. P2.C's batch signature
verification matched that on the crypto path with ~98k verify/s.
Both reports identified the same residual cliff: at
`8t × 800w × 200op` the `manager-write` benchmark dropped to
~3k ops/s — far below either of the in-process ceilings the two
prior phases unlocked.

Profiling traced the cliff to `rope-core::lattice::update_finality`,
which on every new anchor scanned every pending string against
every anchor via a recursive `is_ancestor_of` BFS through the
`parents` DAG:

```
old cost = O(P_pending × A_anchors × D_avg_cone_depth)
```

With 800 wallets fanning into a single lattice and one anchor every
~10 Lamport ticks, that product reached the millions of pointer
chases per anchor creation, and the anchor-creation rate scales
with the append rate.

## What landed

### Per-string anchor-reference watermark

A new per-shard map on `LatticeShard`:

```rust
anchor_refs: RwLock<HashMap<StringId, u32>>
```

stores how many anchors have ever included a given string in their
ancestor cone. Reads are O(1):

```rust
fn count_anchor_references(&self, id: &StringId) -> u32 {
    self.shards[shard_for_string_id(id)]
        .anchor_refs
        .read()
        .get(id)
        .copied()
        .unwrap_or(0)
}
```

### Incremental update path

`check_anchor_creation` now does the work that `update_finality`
used to do, but exactly once per anchor and only on that anchor's
ancestor cone (not on every pending string × every anchor):

```rust
fn increment_anchor_refs_along_cone(&self, anchor_id: StringId) {
    // BFS via per-shard `parents` maps — visits each ancestor once.
    // For every visited string id:
    //   1. shard.anchor_refs[id] += 1
    //   2. if the new count == FINALITY_ANCHORS, finalise:
    //         finalized_strings.insert(id);
    //         shard.pending: drop entry for id;
}
```

Cost analysis:

```
new cost per anchor = O(D_avg_cone_depth)        (BFS)
new cost per finality check = O(1)              (hashmap read)
new cost per finality update = O(D)             (BFS, called once per anchor)
```

Anchor cadence stays at one per ~10 Lamport ticks, so the work
per knot append is amortised at `O(D / 10)` BFS hops — bounded and
constant in the system size.

### Memory hygiene

`mark_erased(id)` now drops the corresponding `anchor_refs`
entry and prunes `pending` of `id` if it was still pending. This
prevents slow growth across GDPR-driven untying loops.

## Benchmark results

Same hardware as P2.B/P2.C (10 logical CPUs, ARM laptop, release
build). Each row averages 5 release-mode runs.

### Memory mode (no fsync)

| Threads | Wallets | Ops total | Throughput | p50 | p99 |
|--------:|--------:|----------:|-----------:|----:|----:|
|       1 |      50 |       200 |     43,216 ops/s | 18.5 µs |  39.2 µs |
|       4 |     200 |       800 |    127,210 ops/s | 24.2 µs |  83.6 µs |
|       8 |     800 |     1,600 |    175,099 ops/s | 40.1 µs | 191.7 µs |
|       8 |     800 |     8,000 |    191,627 ops/s | 23.1 µs | 152.1 µs |
|       8 |   4,000 |     8,000 |    169,766 ops/s | 39.3 µs | 257.5 µs |

### RocksDB mode (full fsync durability)

| Threads | Wallets | Ops total | Throughput | Wait | p50 | p99 |
|--------:|--------:|----------:|-----------:|-----:|----:|----:|
|       8 |     800 |     1,600 |    190,550 ops/s | 0.88 ms | 39.9 µs | 167.8 µs |
|      16 |     800 |    16,000 |    192,033 ops/s | 0.08 ms | 25.1 µs | 822.8 µs |

### The cliff lift

The headline number from the P2.B benchmark report — the
`8t × 800w × 200op` scenario that bottomed out at 3,096 ops/s — now
clears **175,099 ops/s in memory mode and 190,550 ops/s with full
RocksDB fsync durability**. That is a **~57×–62× improvement** on
the exact scenario the cliff was diagnosed against.

The new ceiling is no longer in the lattice path. At 8t × 800w ×
1000op the throughput levels at ~190k ops/s in both memory and
rocksdb modes, indicating the lift from the lattice path is now
larger than either the persistence ceiling (P2.B) or the crypto
verification ceiling (P2.C). Future Phase-2 work scales OUT
across nodes (P2.D) rather than further down inside one node.

### Reproducing

```bash
cargo build --release -p rope-loadgen
BIN=./target/release/rope-loadgen

# The exact scenario the cliff was diagnosed against:
$BIN manager-write -t 8 -o 1600 -w 800 -m memory   --seed 42
$BIN manager-write -t 8 -o 1600 -w 800 -m rocksdb  --await-durable --seed 42
$BIN manager-write -t 8 -o 8000 -w 800 -m memory   --seed 42
$BIN manager-write -t 16 -o 16000 -w 800 -m rocksdb --await-durable --seed 42
```

## Correctness

Per-anchor BFS traverses the same edges as the old
`is_ancestor_of` BFS, just in the opposite direction (from the new
anchor down through `parents` rather than from each pending string
up through `parents`). Both walk every (anchor, ancestor) pair
exactly once across the lifetime of the system, so the final
`anchor_refs` count for any string equals the old
`count_anchor_references` value at any consistent observation
point.

Five new tests pin the contract:

- `anchor_refs_increment_for_genesis_anchor_only` — the genesis
  knot becomes its own first anchor and gets `anchor_refs[id] == 1`.
- `finality_watermark_is_o1_per_string` — older strings accumulate
  at least as many anchor refs as newer ones.
- `finality_threshold_promotes_pending_to_finalized` — when the
  watermark crosses `FINALITY_ANCHORS`, the string moves from
  pending to `finalized_strings`.
- `anchor_refs_dropped_on_mark_erased` — erasure drops the
  watermark entry.
- `check_finality_is_constant_time_after_p2c1` — 1024 finality
  checks across a 256-knot chain complete in < 50 ms (regression
  guard for the O(1) read).

All 79 `rope-core` tests pass (74 existing + 5 new).

## Backward compatibility

The public API is unchanged. `count_anchor_references`,
`check_finality`, `is_finalized`, `update_finality`,
`is_ancestor_of`, and every other surface entry keep their v1.x
signatures and semantics. Callers (notably
`rope-node::ledger_manager`) need no changes.

## Where this puts us in the v2 roadmap

| Phase | Layer | Throughput unlocked | Status |
|---|---|---|---|
| P1.1–P1.5 | Lattice/HLC sharding, OES key cache, RocksDB persistence | ~14k ops/s ceiling lifted | merged |
| P2.A | LedgerLifecycleManager sharding | ~3k ops/s on the cliff scenario | merged |
| P2.B | Parallel WriteBatch consumers | ~110k durable ops/s | shipped on `feat/v2-phase2b-parallel-writebatch` |
| P2.C | Hybrid signature batch verification | ~98k verify/s | shipped on `feat/v2-phase2c-batch-verify` |
| **P2.C.1** | **Lattice finality watermark** | **~190k durable ops/s** | **shipped on `feat/v2-phase2c1-lattice-watermark`** |
| P2.D | Horizontal node sharding | `~190k × N` ops/s | next |
| P2.E | KnotDAG canon change | TBD | next |

## Next: P2.D and P2.E

With the in-process bottlenecks gone, the natural next step is to
scale OUT. Two pieces:

- **P2.D — Horizontal node sharding.** Partition wallets across
  nodes using a deterministic mapping (`wallet_address[0]` →
  `ShardId` → `NodeId`), so independent wallets execute on
  independent nodes in parallel. Linear N× scaling with node count.

- **P2.E — KnotDAG canon change.** Replace the linear personal
  ledger chain with a DAG-of-knots canon. Each knot can have
  multiple parents, enabling concurrent appends within a single
  wallet without head-lock contention. Spec section to be drafted
  alongside the implementation.
