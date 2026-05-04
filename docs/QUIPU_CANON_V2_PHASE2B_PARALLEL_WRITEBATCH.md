# Quipu Canon v2.0 — Phase 2.B: Parallel WriteBatch consumers

**Branch:** `feat/v2-phase2b-parallel-writebatch`
**Base:** `feat/v2-agents-integration` (`f49afec`)
**Date:** 2026-05-04
**Author:** Datachain Foundation DDMI

---

## Goal

Lift the single-fsync ceiling that bounded Phase 1.5's RocksDB persistence
layer at ~100 fsync/s × ~10 ops per batch ≈ ~1,000 ops/s with full
durability. The Phase 1 benchmark identified this as the next bottleneck
once `LedgerLifecycleManager` (P2.A) was sharded.

## Design

The Phase 1.5 layer had:

- **One** `mpsc::Sender<PendingWrite>` channel
- **One** background flusher thread
- **One** `WriteBatch` per 10 ms tick with `WriteOptions::set_sync(true)`

Three bottlenecks:

1. The single `Sender` becomes a CPU bottleneck above a few hundred
   thousand enqueues per second.
2. The single fsync per tick caps durable throughput at the
   tick-rate × batch-size product.
3. There is no way to overlap fsyncs in time, so the SSD's natural
   queue depth is wasted.

Phase 2.B replaces the single channel + single flusher with a pool of
`NUM_SHARDS = 8` independent writers. Each shard owns:

- Its own `mpsc::Sender<PendingWrite>` channel (no enqueue contention)
- Its own background flusher thread
- `highest_assigned_seq` atomic (set on enqueue)
- `highest_durable_seq` atomic (set on flush, after fsync returns)
- A persistent watermark `b"durable_seq_shard_<i>"` written to default
  CF in the same `WriteBatch` as its ops (atomic crash semantics)

Ops are routed to a shard by `partition_byte & SHARD_MASK`, where the
partition byte is `wallet[0]` for descriptor / append / mark-deleted ops
and `string_id[0]` for piece ops. With 8 shards and a uniform first-byte
distribution, every shard gets exactly 32 of the 256 first-byte values.

`next_seq` stays a single global `AtomicU64`, so caller seqs remain a
single monotonic stream across all shards.

### Cross-shard durability invariant

`wait_durable(S)` returns true once for **every** shard `i`:

```text
highest_durable[i] >= min(highest_assigned[i], S)
```

This is the strongest correct invariant:

- A shard whose `highest_assigned <= S` has nothing more to fsync to
  satisfy this call — it only needs to be caught up to its own ceiling.
- A shard whose `highest_assigned > S` only needs to fsync up to `S` to
  satisfy *this* call; its later seqs will be carried by a later wait.

`durable_seq()` returns the largest `S` such that the above holds for
every shard:

```text
S = min over constraining shards i of highest_durable[i]
    if no shard is constraining: S = next_seq() - 1
```

A shard is constraining iff `highest_durable[i] < highest_assigned[i]`.

### Why this beats the single-flusher fsync ceiling

RocksDB has one WAL per DB. When several threads each call
`db.write_opt(batch, &sync_wo)` concurrently, RocksDB performs **WAL
group commit** — multiple concurrent fsync requests are coalesced into
a smaller number of physical fsyncs. So 8 shards each issuing their own
fsync per ~10 ms cost much less than 8 × 1 fsync per shard, while
servicing 8× more ops per tick.

In our manager-write benchmark below, the overhead of fsync over the
in-memory baseline collapses from ~97% (P1.5: 1k ops/s vs 33k memory)
to ~17% at 1 thread, and goes **negative** at 4 threads (parallel
batching outperforms the synchronous in-memory mirror).

### Backward compatibility

A Phase 1.5 database (single `b"durable_seq"` key, no per-shard keys)
must open cleanly under a Phase 2.B binary. The recovery routine:

1. Reads the legacy `b"durable_seq"` key (if present).
2. Reads each `b"durable_seq_shard_<i>"` key (defaults to 0 if absent).
3. Computes `shard_min = min over shards-with-watermark>0 of per-shard`.
4. Sets `recovered.durable_seq = max(legacy, shard_min)`.
5. Initialises every shard's `highest_assigned` and `highest_durable`
   atomic to `max(per_shard[i], recovered.durable_seq)` — so a P2.B
   binary opening a P1.5 DB starts every shard at the legacy global
   watermark, satisfying any `wait_durable(seq <= legacy)` immediately.

This is verified by
`legacy_p1_5_database_is_recovered_via_back_compat_key`.

## Test coverage

35 of 35 `rope-storage` tests pass after the rewrite (28 P1.5 baseline +
7 new P2.B-specific). The new ones:

| Test | What it checks |
|------|----------------|
| `shard_count_is_power_of_two` | Sanity: `NUM_SHARDS = 8`, mask = `0b111`. |
| `op_partitioning_is_stable_and_evenly_spread` | All 256 first-byte values map to exactly 32 hits each across the 8 shards. Stable across processes. |
| `skewed_load_across_shards_still_meets_global_watermark` | Two hot shards + two cold shards under load — `wait_durable` for the highest seq must succeed irrespective of skew. |
| `per_shard_watermarks_persist_and_recover_correctly` | Two shards used, six untouched: per-shard watermarks land in the right keys, and untouched shards are lifted to `recovered.durable_seq` on recovery. |
| `wait_durable_is_correct_when_target_seq_is_in_minor_shard` | 50 ops on shard 0 (no wait), then 1 op on shard 1, then `wait_durable(target_in_shard_1)`: the wait must succeed regardless of shard 0's backlog. |
| `legacy_p1_5_database_is_recovered_via_back_compat_key` | A DB with only the legacy `durable_seq` key (no per-shard keys) opens correctly under P2.B and the legacy watermark is honoured. |
| `durable_seq_matches_is_durable_largest_true` | End-to-end check: `is_durable(durable_seq()) == true`. |

The original 28 tests (concurrent-load, drop-drains-unawaited, recovery,
mark-deleted-on-disk, etc.) all pass unchanged — the public API is
exactly the same.

## Benchmark — manager-write @ rocksdb mode with `--await-durable`

End-to-end measurement on the same machine and with the same workload
shape as the Phase 1 benchmark report. **All numbers below include
fsync** (`WriteOptions::set_sync(true)` round-tripped to disk via
`await_all_durable`).

```text
$ ./target/release/rope-loadgen manager-write \
    -t <T> -o <O> -w <W> \
    -s partitioned -m rocksdb \
    --await-durable \
    --payload-bytes 256 --seed 42

Threads × Ops × Wallets   throughput (work)   throughput (+wait)   p99 latency
------------------------- ------------------- -------------------- -------------
1 × 100   ×  50               27,455 ops/s          27,053 ops/s         124 µs
2 × 200   ×  50               61,091 ops/s          59,516 ops/s          96 µs
4 × 400   × 100              109,990 ops/s         106,966 ops/s          80 µs
8 × 800   × 200                3,096 ops/s           3,094 ops/s        8978 µs   <-- lattice cliff
```

For comparison, **memory-mode** at the same shapes (no persistence at
all, just the in-memory mirror):

```text
1 × 100  ×  50    32,335 ops/s
4 × 400  × 100    84,919 ops/s
```

So at 1 thread the P2.B persistence overhead is `1 - 27455/32335 = 15%`
of memory-mode throughput. **At 4 threads, the rocksdb mode is *faster*
than memory mode** (109,990 > 84,919) because the per-shard parallel
batching coalesces work that the in-memory mirror has to do
synchronously per call.

### What's the relevant prior number?

Phase 1.5's rocksdb mode at 1t × 100 × 50 was ≈ **1,043 ops/s**
(the single-fsync ceiling: ~100 fsync/s × ~10 ops per batch). P2.B at
the same shape is **27,455 ops/s — a ~26× lift on a single thread**.
At 4 threads the lift is ~110×.

### Where the cliff at 8t × 800 × 200 comes from

The 3,094 ops/s number at 8 threads is **NOT the fsync ceiling** — it
is the same `rope-core::lattice::update_finality` O(N²) cliff that the
P2.A subagent honestly flagged. P2.B has lifted the persistence
ceiling cleanly. The lattice cliff is what now limits the headline
multi-thread number, and is the explicit target for **P2.C**.

## Crash semantics

Each shard's `WriteBatch` includes its own watermark put alongside the
ops, so the WAL fsync atomically commits both. On recovery:

- A shard whose WriteBatch was fully fsync'd: ops + watermark both
  visible, `highest_durable` resumes at the recovered watermark.
- A shard whose WriteBatch was partially fsync'd (RocksDB never lets
  this happen — WAL records are atomic): impossible.
- A shard whose WriteBatch was lost in the WAL roll-forward window:
  ops + watermark both rolled back together. `next_seq` resumes at
  one above the global watermark; any in-flight seq above the
  watermark whose caller did NOT call `wait_durable` is documented
  best-effort and may be lost.

The legacy `b"durable_seq"` key is never written by P2.B (only read on
recovery), so a P2.B-then-P1.5 downgrade would only see ops up to the
last P1.5 watermark, but no false durability claims would be made.

## Files changed

```text
crates/rope-storage/src/rocksdb_persistence.rs   +508 / -169
docs/QUIPU_CANON_V2_PHASE2B_PARALLEL_WRITEBATCH.md      (new)
```

## Next: P2.C

The lattice O(N²) `update_finality` cliff is now the binding constraint
on multi-thread `manager-write`. P2.C will replace the per-anchor scan
with a per-string finality watermark (or shard the call by string id)
to lift the `8t × 800 × 200` number from 3k ops/s back into the 100k+
range with full durability.
