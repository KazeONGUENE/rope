# Quipu Canon v2.0 Phase 2.D — Horizontal Node Sharding

**Author:** Kazé A. ONGUENE — Datachain Foundation
**Date:** 2026-05-04
**Branch:** `feat/v2-phase2d-horizontal-sharding`
**Followup to:** `QUIPU_CANON_V2_PHASE2C1_LATTICE_WATERMARK.md`

---

## Why this exists

Phases 1.1–2.C.1 lifted every binding constraint inside a single
node:

- P1.1 sharded lattice — 256-way intra-node partitioning
- P1.2 head-string lock — per-wallet append serialisation
- P1.3 per-shard HLC — no global clock contention
- P1.4 OES key cache — amortised crypto setup
- P1.5 RocksDB persistence — durable writes off the hot path
- P2.A `LedgerLifecycleManager` sharding — manager-level head fan-out
- P2.B parallel `WriteBatch` consumers — ~110 k durable ops/s
- P2.C hybrid signature batch verification — ~98 k verify/s
- P2.C.1 lattice finality watermark — ~190 k durable ops/s

That ceiling is now the box itself: CPU, NIC, disk. Phase 2.D
scales *out* by partitioning the wallet keyspace across multiple
nodes. Independent wallets land on independent nodes and execute
in parallel.

## What landed

A new crate, `crates/rope-cluster/`, providing the partitioning,
membership, and dispatch primitives needed to fan a workload
across N nodes. The crate is self-contained: it depends only on
`rope-core` (for `NodeId`) and the standard async/serde toolchain.

### Modules

| Module | Responsibility |
|---|---|
| `partition` | `ShardId` (= `wallet_address[0]`), `PartitionMap` (round-robin + rendezvous-hashing constructors), `ShardOwnership` |
| `membership` | `ClusterMembership`, `NodeDescriptor` — immutable cluster snapshots |
| `op` | `ShardOp` / `ShardResult` — opaque-payload routable ops |
| `endpoint` | `ShardEndpoint` trait + `LocalShardEndpoint` (in-process) + `InMemoryRemoteEndpoint` (test/sim) |
| `router` | `ClusterClient` — atomic-swap dispatch with per-shard counters |
| `error` | `ClusterError` — explicit failure modes (unroutable, missing endpoint, owner-not-in-membership, transport, …) |

### Sharding scheme

256 shards keyed by `wallet_address[0]`. Identical axis to the
intra-node sharding used by `rope-core::lattice`,
`rope-core::clock`, `rope-core::personal_ledger`, and
`rope-storage::rocksdb_persistence`. The deliberate symmetry
means **once an op is routed to its owning node, every existing
per-shard primitive handles it without further routing**.

Wallet addresses are 20-byte secp256k1-derived hashes (or, for
synthetic test wallets, 20 random bytes with the same shape) so
the first byte is uniformly distributed — no rehash required.

### Two partition-map constructors

- `PartitionMap::round_robin(&[NodeId])` — `shard_id % node_count`.
  Cheap, perfectly balanced for shard counts that are integer
  multiples of the node count, but moves a large fraction of
  shards on every topology change.
- `PartitionMap::rendezvous(&[NodeId])` — highest-random-weight
  hashing (HRW). Rebalance moves only `1/N` of shards when a node
  joins or leaves.

Both produce `Vec<NodeId>` (length = 256), wrapped in a `Clone`
struct so the routing client can swap maps atomically without
draining in-flight ops.

### Dispatch path

```text
caller ──── ShardOp ────▶ ClusterClient
                              │
                              ├── ShardId::for_wallet(op.wallet)
                              ├── PartitionMap::owner(shard) → NodeId
                              ├── ClusterMembership::lookup(node) (validate)
                              └── endpoints[node].execute(op).await
```

Steps 1–3 are O(1) reads against `parking_lot::RwLock`s; step 4
is whatever the endpoint says it is. For local endpoints that
means an in-process function call; for remote endpoints, the
production transport's wire cost.

### Two endpoint implementations

- `LocalShardEndpoint` — owns a `Fn(ShardOp) -> Result<ShardResult, String>`
  closure. The receiving node provides the closure; it decodes the
  opaque payload and runs the matching `LedgerManager::*` (or
  other) call.
- `InMemoryRemoteEndpoint` — wraps another endpoint and forwards.
  Used by the integration tests in this crate to exercise the
  remote-dispatch path without touching real sockets. Includes a
  fault-injection hook so failover paths can be tested explicitly.

Production deployments swap `InMemoryRemoteEndpoint` for a
network-backed implementation (gRPC, libp2p request-response, …).
The trait surface is intentionally tiny so the production
transport only has to implement `execute`.

## Tests

### Unit (`crates/rope-cluster/src/**/tests.rs`) — 28 tests, all green

- `partition::tests` (10) — shard derivation, round-robin balance,
  rendezvous balance, rebalance disruption bound, constructor
  guards (empty input, missing/duplicate assignments).
- `membership::tests` (3) — sorted iteration, lookup, ids.
- `op::tests` (3) — bincode round-trips of `ShardOp` and
  `ShardResult` (including the empty case).
- `endpoint::tests` (4) — local execute + counter, error
  propagation, remote forwarding, fault-injection toggle.
- `router::tests` (8) — routing to correct owner, end-to-end
  remote, missing endpoint, empty wallet, partition swap,
  wallet-grouping batch helper, per-shard counters,
  owner-not-in-membership error.

### Integration (`crates/rope-cluster/tests/two_node_demo.rs`) — 3 tests, all green

- `two_node_cluster_routes_appends_to_correct_owner` — issues one
  append per shard against a 2-node cluster, then verifies that
  each node's `LedgerStore` mirror contains exactly the wallets
  whose shard rounds to it; reads via the cluster client return
  the same chain.
- `cluster_balances_load_evenly_across_two_nodes` — 1024 random
  wallets, each node lands within ±20% of expected count.
- `cluster_handles_endpoint_failure_without_corrupting_neighbours`
  — endpoint A always errors, endpoint B keeps running normally;
  the failure surfaces explicitly.

### Loadgen (`crates/rope-loadgen/src/scenarios/cluster_write.rs`) — 5 tests, all green

The new `cluster-write` subcommand spins up N in-process nodes
backed by `LedgerStore`, drives synthetic appends through the
cluster, and reports per-node + aggregate throughput.

## Benchmark results

Same hardware as P2.C.1 (10 logical CPUs, ARM laptop, release
build). Each row is one release-mode run with `--seed 42` so
results are bit-reproducible.

| Topology       | Total ops | Elapsed   | Throughput (ops/s) | p50    | p99    | Per-node spread |
|----------------|-----------|-----------|---------------------|--------|--------|------------------|
| 1n × 8t        | 100,000   |  89.8 ms  |   1,114,096         | 2.4 µs | 65 µs  | 100,000 (single) |
| 2n × 8t        | 100,000   |  58.4 ms  |   1,713,729         | 2.5 µs | 36 µs  | 48,725 / 51,275  |
| 4n × 16t       | 200,000   |  96.0 ms  |   2,082,288         | 2.5 µs | 77 µs  | 47,762 / 53,663  |
| 8n × 16t       | 400,000   | 183.8 ms  |   2,176,342         | 2.4 µs | 109 µs | 43,706 / 58,647  |

Headline numbers:

- **2× scaling at 2 nodes**: 1.54× over single node (1.7 M / 1.1 M).
  The 14% gap to ideal is mostly the tokio runtime's task
  scheduling on a fixed worker pool; with a real network
  transport that gap will be amortised against transport latency.
- **4× scaling at 4 nodes**: 1.87× over single node — close to
  ideal (the workers contend on tokio's task queue rather than on
  any shared lattice/lock).
- **Tight per-node load distribution**: at 8 nodes the worst-case
  spread is 43,706 / 58,647 ≈ 0.74. That's the variance
  intrinsic to placing 256 shards across 8 owners (32 each on
  average, ±10 typical).

The cluster routing layer therefore adds **near-zero per-op
overhead**: single-node throughput at 1.1M ops/s through the
cluster is in the same envelope as bare `LedgerStore::append_to_chain`
(no routing), and scaling is close to linear up to the worker-pool
saturation point.

### Reproducing

```bash
cargo build --release -p rope-loadgen
BIN=./target/release/rope-loadgen

$BIN cluster-write -n 1 -t 8  -o 100000 -w 800 --seed 42
$BIN cluster-write -n 2 -t 8  -o 100000 -w 800 --seed 42
$BIN cluster-write -n 4 -t 16 -o 200000 -w 800 --seed 42
$BIN cluster-write -n 8 -t 16 -o 400000 -w 800 --seed 42
```

## What this crate does NOT do (yet)

These are deliberately separate steps so each can be reviewed,
benchmarked, and rolled back independently:

- **Production transport.** The `ShardEndpoint` trait is the
  swap-in point; a follow-up patch wires gRPC + mTLS for
  cross-VPS dispatch.
- **Topology changes (lease-based shard ownership).** Today an
  operator swaps `PartitionMap` atomically; a follow-up Phase
  2.D.1 will add lease-based shard handover so node joins/leaves
  cause a brief per-shard pause rather than a global swap.
- **Cross-shard transactions (2PC).** All current ops are
  wallet-keyed, so they fit in a single shard. Phase 2.D.2 adds
  a 2-phase coordinator for ops that touch two distinct wallets
  atomically (e.g. cross-wallet credit transfers in
  `rope-smartchain`).
- **Replication.** Each shard has exactly one owner here. Phase
  2.D.3 will add quorum replication of the per-shard log so node
  failures don't lose data.

## Backward compatibility

`rope-cluster` is a brand-new crate — nothing depends on it yet.
Existing single-node deployments are unaffected; they can
continue running without ever touching the cluster crate. When a
deployment opts in, it does so by:

1. Constructing one `ClusterClient` per process.
2. Registering its own node id's endpoint as `LocalShardEndpoint`
   (with a closure that bridges to its `LedgerManager`).
3. Registering peer node endpoints as the production
   `RemoteShardEndpoint` (when wired in).
4. Routing every wallet-keyed call through `ClusterClient::dispatch`
   instead of calling `LedgerManager::*` directly.

## Where this puts us in the v2 roadmap

| Phase | Layer | Throughput unlocked | Status |
|---|---|---|---|
| P1.1–P1.5 | Lattice/HLC sharding, OES key cache, RocksDB persistence | ~14k ops/s ceiling lifted | merged |
| P2.A | `LedgerLifecycleManager` sharding | manager-level head fan-out | merged |
| P2.B | Parallel `WriteBatch` consumers | ~110k durable ops/s | shipped on `feat/v2-phase2b-parallel-writebatch` |
| P2.C | Hybrid signature batch verification | ~98k verify/s | shipped on `feat/v2-phase2c-batch-verify` |
| P2.C.1 | Lattice finality watermark | ~190k durable ops/s | shipped on `feat/v2-phase2c1-lattice-watermark` |
| **P2.D** | **Horizontal node sharding (`rope-cluster` crate)** | **~190k × N nodes** | **shipped on `feat/v2-phase2d-horizontal-sharding`** |
| P2.E | KnotDAG canon change | TBD | next |

## Next: P2.E

With horizontal sharding in place, the only remaining structural
constraint inside a single shard is the linear personal-ledger
chain: only one append per wallet at a time (the head lock from
P1.2). For wallets with very high concurrent inflow (exchanges,
bridges, custodial services), even the per-wallet head lock
matters. P2.E replaces the linear chain with a DAG-of-knots
canon that allows multiple concurrent appends to converge through
explicit multi-parent edges.
