# Quipu Canon v2.0 Phase 2.E — KnotDAG Canon

**Author:** Kazé A. ONGUENE — Datachain Foundation
**Date:** 2026-05-04
**Branch:** `feat/v2-phase2e-knot-dag-canon`
**Followup to:** `QUIPU_CANON_V2_PHASE2D_HORIZONTAL_SHARDING.md`

---

## Why this exists

P1.2 added per-wallet head-string locks so concurrent appends to
**different** wallets no longer serialise. P2.D scaled OUT
across nodes so independent wallets execute on independent
machines. But within one wallet, only one append could be in
flight at a time — the head lock still serialised. For wallets
with high inflow (exchanges, bridges, custodial services, IoT
gateways) that per-wallet ceiling matters.

Phase 2.E lifts it. The canonical wallet history changes shape:

```
v1.x linear chain                    v2 KnotDAG canon
─────────────────                    ────────────────
G ← A ← B ← C ← D                    G ← A ──┐
                                       ← B ──┤
                                       ← C ──┴── D
```

A wallet's "head" becomes a **set of tips** rather than a single
id. Two appends that race against the same tip set both succeed;
their resulting knots both reference the same parents (becoming
siblings); the next append references both as parents and the
wallet history re-converges into a single tip.

## What landed

### `crates/rope-core/src/knot_dag.rs`

A standalone, in-memory, per-wallet DAG primitive plus a 256-shard
registry of per-wallet DAGs. Self-contained — no dependency on
the global lattice's locks.

### `KnotDag` API

```rust
pub struct KnotDag { /* parents, children, tips */ }

impl KnotDag {
    pub fn new() -> Self;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;

    // Tip-set queries — O(1).
    pub fn tips(&self) -> Vec<StringId>;
    pub fn is_tip(&self, id: &StringId) -> bool;

    // Topology queries.
    pub fn contains(&self, id: &StringId) -> bool;
    pub fn parents_of(&self, id: &StringId) -> Vec<StringId>;
    pub fn children_of(&self, id: &StringId) -> Vec<StringId>;

    // The mutation point — atomic; updates the tip set in one
    // critical section.
    pub fn add_knot(&self, id: StringId, parents: &[StringId])
        -> Result<(), KnotDagError>;

    // Traversals.
    pub fn ancestors(&self, start: &StringId) -> Vec<StringId>;
    pub fn descendants(&self, start: &StringId) -> Vec<StringId>;
    pub fn topo_sorted(&self) -> Vec<StringId>;

    // Invariants & serialisation.
    pub fn is_consistent(&self) -> bool;
    pub fn snapshot(&self) -> KnotDagSnapshot;
}
```

### `KnotDagRegistry` — sharded multi-wallet DAG

```rust
pub const KNOT_DAG_NUM_SHARDS: usize = 256;

pub struct KnotDagRegistry { /* 256 per-wallet shards */ }

impl KnotDagRegistry {
    pub fn new() -> Self;
    pub fn dag_for(&self, wallet: &[u8]) -> Arc<KnotDag>;
    pub fn contains(&self, wallet: &[u8]) -> bool;
    pub fn wallet_count(&self) -> usize;
    pub fn append(&self, wallet: &[u8], id: StringId, parents: &[StringId])
        -> Result<(), KnotDagError>;
}
```

Sharding axis = `wallet_address[0]`. Identical to the lattice
(P1.1), the HLC (P1.3), the head-lock pool (P1.2), the parallel
WriteBatch consumers (P2.B), the lattice finality watermark
(P2.C.1), and the cluster partition map (P2.D). Once a wallet is
in the registry, its `Arc<KnotDag>` can be passed around and
called concurrently with other wallets on the same shard with
zero registry-level contention.

### Errors

```rust
pub enum KnotDagError {
    UnknownParent { parent: StringId },   // cross-wallet edge attempted
    DuplicateKnot { id: StringId },       // id already present
    CycleDetected { id, parents },        // self-loop / future-proofing
}
```

## Tests — 16 unit, all green

| Test | What it pins |
|---|---|
| `empty_dag_has_no_knots_no_tips` | construction invariants |
| `single_knot_becomes_genesis_tip` | first-knot path |
| `linear_chain_keeps_one_tip` | v1 backward compat |
| `fork_creates_two_tips` | multi-parent fan-out |
| `merge_collapses_tips_back_to_one` | multi-parent re-convergence |
| `diamond_topology_is_detected_and_traversed` | non-trivial DAG shape |
| `topo_sort_respects_partial_order` | Kahn's algorithm correctness |
| `unknown_parent_is_rejected` | error path |
| `duplicate_knot_is_rejected` | error path |
| `self_loop_is_rejected_as_cycle` | cycle defence |
| `concurrent_appends_to_same_wallet_succeed` | atomicity under write-write race |
| `snapshot_round_trips_through_serde` | wire format |
| `registry_creates_dag_lazily` | lazy creation, no upfront cost |
| `registry_isolates_wallets` | cross-wallet independence |
| `registry_concurrent_appends_to_distinct_wallets_dont_block` | per-wallet sharding |
| `registry_concurrent_appends_to_same_wallet_via_dag_arc` | the Arc-then-append pattern |

Plus 5 loadgen tests for the `dag-write` subcommand, including a
**barrier-coordinated fan-out test** that proves N threads racing
on the same `tips()` snapshot all commit, producing N concurrent
tips (where the prior linear-chain canon would have serialised
all N).

## `dag-write` benchmark

New `rope-loadgen dag-write` subcommand drives synthetic appends
against `KnotDagRegistry`. Hardware: 10 logical CPUs, ARM
laptop, release build, `--seed 42`.

| Scenario                              | Throughput      | p50    | p99    |
|---------------------------------------|-----------------|--------|--------|
| 8t × 100k × 256w random               |  **7,040,332/s** | 0.46 µs |  5.6 µs |
| 16t × 1M  × 1024w random              |  **6,768,546/s** | 0.54 µs |  3.1 µs |
| 16t × 100k × 1w  `--single-wallet`    |    830,636/s   | 0.46 µs | 164 µs |
| 32t × 1M  × 1w  `--single-wallet`     |    615,430/s   | 0.54 µs | 419 µs |

Headlines:

- **~7 M ops/s** on the realistic mixed scenario — the
  per-wallet DAG is essentially memory-speed once the wallet's
  shard is hot in cache.
- **~600–830 k ops/s** even when 16–32 threads all slam a
  single wallet. Under the v1 linear-chain canon every one of
  those threads would take the same head lock and serialise; here
  they each commit a sibling in their own critical section.
- **Sub-µs p50** across all scenarios — the DAG mutation is just
  a handful of hashmap inserts under one `parking_lot::RwLock`
  write.

The `dag-write` benchmark deliberately exercises **only** the
DAG primitive, not the full append pipeline. Once the
`LedgerManager` integration lands (P2.E.1), the end-to-end
ceiling will be the union of the DAG ceiling, the persistence
ceiling (P2.B), and the crypto ceiling (P2.C) — but the binding
constraint will no longer be per-wallet head-lock contention.

### Reproducing

```bash
cargo build --release -p rope-loadgen
BIN=./target/release/rope-loadgen

# Realistic mixed.
$BIN dag-write -t 8  -o 100000  -w 256  --seed 42
$BIN dag-write -t 16 -o 1000000 -w 1024 --seed 42

# Worst-case head-lock contention.
$BIN dag-write -t 16 -o 100000  -w 1 --single-wallet --seed 42
$BIN dag-write -t 32 -o 1000000 -w 1 --single-wallet --seed 42
```

## What this module does NOT do (yet)

- **`LedgerManager` integration.** This is a standalone primitive
  with full tests. The P2.E.1 follow-up wires it into
  `LedgerManager::append_to_dag` (parallel to the existing
  `append_to_ledger` linear-chain entry point) so callers can opt
  in per-wallet.
- **DAG-aware persistence.** The DAG is in-memory. Disk-backed
  snapshots will reuse `rope-storage::WriteOp` once the
  `LedgerManager` integration lands.
- **DAG-aware finality.** This module exposes the topology only.
  Each knot still finalises via the global lattice's per-string
  finality watermark (P2.C.1). A future P2.E.2 may add
  DAG-aware confirmation rules (e.g. "finalised when all paths
  to the knot have an anchor").
- **Cross-wallet edges.** A knot may only reference parents in
  the same wallet's DAG. Cross-wallet causal edges remain in the
  global lattice.

## Backward compatibility

Brand-new module — nothing depends on it. Existing
`personal_ledger::LedgerChain` callers continue to operate
unchanged. When a deployment opts in:

1. Construct `KnotDagRegistry::new()` once per node (or per
   shard, when wired through `rope-cluster`).
2. On every append, call `registry.append(wallet, id, &dag.tips())`.
3. On every read of "wallet head", call `dag.tips()` and accept
   that the answer may be a SET rather than a single id.
4. Linear-view consumers (Datawallet+ UI, regulatory exports)
   call `dag.topo_sorted()` to render the DAG as an ordered list.

## Where this puts us in the v2 roadmap

| Phase | Layer | Throughput unlocked | Status |
|---|---|---|---|
| P1.1–P1.5 | Lattice/HLC sharding, OES key cache, RocksDB persistence | ~14 k ops/s ceiling lifted | merged |
| P2.A | `LedgerLifecycleManager` sharding | manager-level head fan-out | merged |
| P2.B | Parallel `WriteBatch` consumers | ~110 k durable ops/s | shipped on `feat/v2-phase2b-parallel-writebatch` |
| P2.C | Hybrid signature batch verification | ~98 k verify/s | shipped on `feat/v2-phase2c-batch-verify` |
| P2.C.1 | Lattice finality watermark | ~190 k durable ops/s | shipped on `feat/v2-phase2c1-lattice-watermark` |
| P2.D | Horizontal node sharding (`rope-cluster` crate) | ~190 k × N nodes | shipped on `feat/v2-phase2d-horizontal-sharding` |
| **P2.E** | **KnotDAG canon** | **~7 M DAG ops/s; per-wallet head-lock removed** | **shipped on `feat/v2-phase2e-knot-dag-canon`** |
| P2.E.1 | `LedgerManager::append_to_dag` integration | end-to-end DAG-mode appends | next |
| P2.E.2 | DAG-aware finality rules | reduced per-knot anchor count | next |

## Closing note

With P2.C.1, P2.D, and P2.E in flight together, every binding
constraint identified at the end of P2.B is now lifted:

- The single-node lattice path no longer drops at the
  finality-update O(N²) cliff (P2.C.1: ~190 k durable ops/s).
- Single-node throughput scales OUT across machines via
  `rope-cluster` (P2.D: ~190 k × N).
- Per-wallet appends no longer serialise on a single head lock
  (P2.E: ~7 M DAG-only ops/s, ~600 k–830 k under worst-case
  single-wallet contention).

The path from here to "several million transactions per second"
is now a deployment problem (provision N nodes, wire the
production transport, deploy the `LedgerManager::append_to_dag`
integration) rather than a research problem.
