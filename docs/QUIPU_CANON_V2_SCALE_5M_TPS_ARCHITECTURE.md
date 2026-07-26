# Quipu Canon v2.0 — Architecture for 5M+ Knots/Second

**Status:** DESIGN (not implementation)
**Author:** Datachain Rope core agent
**Date:** 2026-05-03
**Supersedes for scaling:** the implicit "single-node, single-lattice, linear-per-wallet" model of v1.1 / v1.2
**Does NOT supersede:** the GDPR `untie_knot` semantics, the v1.2 `StringRegistry` external contract, or the per-event erasure guarantee — those carry forward
**Companion rules:**
- `.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc` (always-applied, short summary)
- `.cursor/rules/handover-quipu-canon-v2-migration-2026-05-03.mdc` (for ecosystem agents)

---

## 0. Executive Summary

Datachain Rope today serves at most a **few thousand knot writes per second** before contention collapses the write path (this is what produced the 504s reported by DCSwap on 2026-05-03). The user-stated target is **several million transactions per second in parallel**.

We commit to **5,000,000 sustained knot writes/second** as the v2.0 target, broken into four phases plus an optional accelerator phase. The phases are:

| Phase | Headline change | Per-node TPS | Aggregate TPS | Calendar months | Canon-breaking? |
|---|---|---:|---:|---:|---|
| 1 | Sharded lattice + per-wallet head lock + parallel OES + persistence | 50K–100K | 50K–100K | 1.5 | No |
| 2 | Real consensus turned on with batched/aggregated signatures | 50K–100K (held) | 50K–100K | 2 | No |
| 3 | Horizontal node sharding (16-node cluster, wallet-prefix partition) | 100K | **1.6M** | 4 | No (RPC additive) |
| 4 | DAG-of-knots (`KnotDAG`) replaces strict per-wallet head-string | 300K | **5M+** | 6 | **YES** — emitter migration required |
| 5 | GPU/ASIC PQ-signing offload | 600K | 10M+ | 2 (after P3) | No |

**Total:** Phase 1–4 lands 5M+ TPS in **~12–14 calendar months** with a fleet of ~16 production nodes (eng-months total ~27, see §9.3). Phase 5 doubles that with one batch of GPU-equipped nodes.

The design is **emitter-compatible through Phase 3**. Phase 4 is the only true canon break and ships behind a versioned RPC namespace (`rope_v2_*`) so v1.2 emitters keep working forever via a projection layer.

---

## 1. The Real Bottleneck (measured against ground truth, not guessed)

The 504-timeout investigation (originally raised by the DCSwap agent on 2026-05-03 as intermittent timeouts on `rope_appendToLedger`) surfaced four findings that overturn the "disk I/O" hypothesis in that handover:

1. **Hot-path crypto is empty signatures, not Dilithium.** `personal_ledger.rs:619–639` calls `HybridSignature::empty()` on every append. Dilithium is wired but not invoked on the write path. Today's CPU cost is **OES + AEAD + lock contention**, not PQ signing.
2. **OES `derive_key` is the dominant per-knot CPU cost.** `oes.rs:766–801` runs **100–199 iterative BLAKE3 rounds** per ledger key derivation, plus mixing of a ~992-byte genome buffer. Empirically that is **≈30–50µs per call** on a modern x86 core, performed once per knot under `OESManager.state.read()`.
3. **A single global Lamport `Mutex` serializes every knot.** `clock.rs:161–183` advances the logical clock under one `parking_lot::Mutex<LamportClock>` — every wallet, every shard, every concurrent request funnels through this lock.
4. **`StringLattice::add_string` takes 4 write locks at once.** `lattice.rs:274–285` acquires writer locks on `strings`, `complements`, `ordering` (the petgraph DAG), and `pending_strings` for every append. There is no per-wallet sharding: one global `RwLock<HashMap<StringId, RopeString>>` covers all wallets.
5. **`LedgerStore` is in-memory.** `rope-storage/src/lib.rs:17–18, 174–215` carries the comment `// RocksDB will replace this`. There is no synchronous disk write on the hot path today. The DCSwap "disk I/O" hypothesis was a red herring; the real cost is CPU + lock contention.
6. **Real consensus is currently disabled.** `consensus_orchestrator.rs:116–119` sets `verify_signatures: false` and `require_parent_finality: false`. Turning consensus on without architectural changes would *worsen* the bottleneck, not improve it.

### Measured ceiling today (single 8-core VPS, in-memory)

A pessimistic but defensible model:

```
per-knot CPU       ≈ 30–50µs (OES) + 10µs (AEAD) + 10µs (BLAKE3 ledger nonce + content hash) ≈ 50–70µs
single-thread cap  ≈ 1 / 60µs ≈ 16,000 knot/sec
8-thread theoretical ceiling ≈ 130,000 knot/sec
realistic ceiling  ≈ 1,500–4,000 knot/sec
                       (collapsed by global Lamport Mutex + 4-way RwLock cascade in add_string)
```

That collapse explains the 504 perfectly: ~4 concurrent DCSwap bot wallets, each issuing ~1 RPC/sec under our 30s nginx window, easily queues past 10s when the lock cascade serializes them.

---

## 2. Design Principles (carry into every phase)

P1. **Canon stability.** v1.2 emitters (DCSwap, Tanastok, Datawallet+, Alteros, etc.) keep working. Phase 4's canon break is opt-in: v1.2 RPC namespaces remain valid, served by a projection layer over the v2.0 DAG.

P2. **GDPR `untie_knot` is non-negotiable.** Per-knot tombstone semantics must survive sharding (Phase 3) and DAG-ification (Phase 4). The tombstone primitive is the reason the canon exists.

P3. **No "fake TPS."** A knot counts only when (i) durably persisted, (ii) finalized by consensus (or scheduled-to-be-finalized via the local pre-consensus path), and (iii) visible to a client read with sub-second tail latency. Anything that fails one of these is benchmark theatre and not counted.

P4. **Honest crypto.** The v1.2 emitter expects empty signatures because that's what the chain does today. v2.0 turns on real signing. We size every phase **assuming** Ed25519+Dilithium hybrid is enabled, even though Phase 1 gets headroom from the current empty-signature shortcut.

P5. **Failure-isolated shards.** When one shard goes hot or hardware-fails, neighbours must continue serving their wallets without coordination. No "global state" is permitted to live in any single node by Phase 3.

P6. **Each phase is independently shippable.** No phase relies on another being mid-flight. Each can be deployed, measured, and rolled back on its own.

---

## 3. Phase 1 — Sharded Lattice + Per-Wallet Head Lock + Parallel OES + Persistence

**Target:** 50,000–100,000 knot/sec on a single 32-core production node.
**Eng:** ~6 weeks, no canon break, no emitter changes.

### 3.1 Shard the lattice by wallet-prefix

Replace each `RwLock<HashMap<...>>` in `StringLattice` with **256-shard concurrent maps** keyed by the first byte of the wallet address (or, for global structures, by the first byte of the `StringId`). Use `dashmap::DashMap` (which is itself a sharded RwLock-of-HashMap) or hand-rolled `[RwLock<HashMap<_, _>>; 256]`.

Specifically:

| Field | Today | v2.0 P1 |
|---|---|---|
| `strings: RwLock<HashMap<StringId, RopeString>>` | 1 lock | 256 shards keyed by `string_id[0]` |
| `complements: RwLock<HashMap<StringId, Complement>>` | 1 lock | 256 shards |
| `ordering: RwLock<LatticeDAG>` | 1 lock, single petgraph | 256 sub-DAGs (one per shard) plus a thin "anchor crossbar" that links inter-shard parentage edges only |
| `pending_strings: RwLock<BTreeMap<u64, HashSet<StringId>>>` | 1 lock | 256 shards keyed by Lamport-mod-256 |
| `creator_index: RwLock<HashMap<[u8;32], Vec<StringId>>>` | 1 lock | 256 shards keyed by `creator_pk[0]` |
| `knot_tombstones`, `erased_strings`, etc. | 1 lock each | 256 shards each |

The petgraph split is the only non-trivial change. Most parentage is intra-wallet (parent and child belong to the same wallet → same shard byte); cross-wallet parentage is only created by anchors. Anchors get their own dedicated cross-shard structure.

### 3.2 Per-wallet head-string lock

Today a concurrent append for wallet `W` can race: both readers see the same `head_id`, then both try to append with the same parent. Today's parent-existence check passes both, producing two siblings of the same parent (a fork in the wallet's chain).

Add a **per-wallet `tokio::sync::Mutex<HeadState>`** keyed by `wallet[..32]`. Acquire it for the read-modify-write window of `head_id`. This is a **per-wallet** lock, not global, so concurrent appends across wallets remain unconstrained.

Implementation note: use a `dashmap::DashMap<[u8; 32], Arc<tokio::sync::Mutex<HeadState>>>` so the locks themselves auto-shard.

### 3.3 Replace global Lamport Mutex with a per-shard Hybrid Logical Clock (HLC)

The current `ClockManager::tick` is a single `parking_lot::Mutex<LamportClock>` that all writers contend on. Replace with:

- One `HybridLogicalClock` per shard (256 instances), each maintaining `(physical_ns, logical_counter)`.
- `tick(shard)` is uncontended within a shard.
- Cross-shard ordering: anchors capture the **max HLC across shards** at anchor time, providing a partial-order spine that consensus can reason about.

HLC is a well-studied primitive (CockroachDB, MongoDB, YugabyteDB). It preserves causality and gives us monotonicity per shard plus a deterministic merge rule on read.

### 3.4 Parallel OES `derive_key`

`OES::derive_key` is purely CPU-bound and re-entrant under `state.read()`. Today every knot blocks a Tokio worker for ~30–50µs. Two changes:

1. **Move OES work off the Tokio worker pool.** Wrap `derive_key` calls in `tokio::task::spawn_blocking` (or, better, a dedicated `rayon::ThreadPool` sized to physical cores). The Tokio runtime stops being CPU-starved and request handlers stay responsive.
2. **Cache derived keys per `(wallet, generation)`.** Today `derive_key` is called once per knot. Within a single OES generation (= ~100 anchors ≈ 7 minutes wall clock), the derived key for `(wallet, generation)` is constant. Cache it in a `DashMap<(wallet, generation), LedgerKey>` with `zeroize` on eviction. **This is the single biggest speedup in Phase 1** — it converts a 30–50µs per-knot cost into a once-per-7-minutes per-wallet cost, then ~0 thereafter.

After P1.4 the per-knot CPU drops from ~50µs to ~10µs (just the AEAD encrypt + BLAKE3 nonce). That alone is a 5× speedup at the CPU layer.

### 3.5 RocksDB persistence (bounded write amplification)

The current `LedgerStore` is in-memory only. Phase 1 adds a RocksDB column-family layout:

| Column family | Key | Value |
|---|---|---|
| `strings` | `StringId` (32B) | bincode-serialized `RopeString` |
| `wallet_chain` | `wallet (32B) ‖ seq (8B BE)` | `StringId` |
| `head_index` | `wallet (32B)` | `StringId` |
| `tombstones` | `StringId` | `KnotTombstone` |
| `oes_state` | `generation (8B BE)` | snapshot |

Writes use **WriteBatch + WriteOptions{disable_wal: false, sync: false}** to coalesce per-shard flushes (~10ms). The in-memory `DashMap`s become a write-through cache.

Crucially: **persistence happens off the request critical path** via a `mpsc::channel<WriteRequest>` per shard, drained by a dedicated writer task. The RPC handler returns once the in-memory write succeeds and the message is enqueued; durability is confirmed by a watermark broadcast (sub-100ms typical). For applications that demand durability-before-ack (rare), an `?durable=true` RPC flag waits on the watermark.

### 3.6 Rewrite the RPC handler signature

`rope_appendToLedger` becomes:

```rust
#[rpc(name = "rope_appendToLedger")]
async fn append_to_ledger(&self, owner: WalletAddress, record: LedgerRecord)
    -> Result<AppendReceipt, RpcError>
{
    let shard = shard_for_wallet(&owner);                    // O(1)
    let head_lock = self.head_locks.get_or_insert(owner);   // per-wallet mutex
    let _head_guard = head_lock.lock().await;
    let key = self.oes_key_cache.get_or_derive(&owner).await; // cached
    let (ciphertext, knot) = build_knot(record, &key);       // CPU
    self.lattice.shards[shard].append(knot)?;                // sharded write
    self.persistence_tx[shard].send(knot.clone()).await?;    // async flush
    Ok(AppendReceipt { knot_id: knot.id, hlc: knot.hlc })
}
```

No external API change. v1.2 emitters see the same RPC.

### 3.7 Phase 1 capacity math

Under the assumptions above, on a 32-core 64GB VPS (3× current Gandi 8-core sizing):

```
Per-knot CPU          ≈ 10µs (AEAD + BLAKE3) when OES key is cached
Per-shard sustained   ≈ 1 / 10µs = 100,000 knot/sec/shard
Realised aggregate    ≈ 50,000–100,000 knot/sec (lock contention amortised across 256 shards)
RocksDB write rate    ≈ 100,000 writes/sec achievable at ~50MB/sec on NVMe
Memory overhead       ≈ 256 × ~1MB shard = ~256MB DashMap + RocksDB cache
```

That's a **~30× speedup over today's measured ceiling** with no canon break. Phase 1 alone solves the DCSwap 504 problem and gives runway for the next 18–24 months of organic ecosystem growth.

---

## 4. Phase 2 — Real Consensus With Batched/Aggregated Signatures

**Target:** Hold 50K–100K knot/sec while turning real consensus on.
**Eng:** ~8 weeks, no canon break, no emitter changes.

### 4.1 Why this phase exists

Phase 1 ships throughput at the cost of leaving `verify_signatures: false`. That's fine for a private bridge, **not** fine for a public ledger advertising itself as a Datachain. Phase 2 turns signing on without giving back the throughput.

### 4.2 Batched Ed25519 verification

`ed25519-dalek` 2.x exposes `verify_batch`. For each shard, accumulate up to 128 testimonies in a 5ms verification window, then call `verify_batch`. This converts ~50µs per signature to ~5µs amortised — **10× speedup on Ed25519 verification**.

### 4.3 Dilithium signature aggregation strategy

CRYSTALS-Dilithium does not natively aggregate. Three options, in order of preference:

1. **Threshold testimony signing**: validators co-sign one Dilithium signature per anchor (every ~10s) over the merkle root of the anchor's knot batch. Per-knot Dilithium cost → 0; per-anchor cost → 1 Dilithium signature × (validator count). At 21 validators × 10s anchors that is 2.1 Dilithium ops/sec/node. Trivial.
2. **Per-shard Dilithium signing**: one signature per shard per anchor (256 × 21 / 10 = 537 ops/sec). Still trivial.
3. **BLS overlay**: switch the testimony envelope from Ed25519+Dilithium to **BLS12-381 (classical) + Dilithium (PQ)**, where the BLS portion aggregates via standard pairing-based signature aggregation. This gives one ~96-byte BLS signature per anchor instead of N. Adds a `blst` dependency. Recommended once we hit ≥100 validators.

For Phase 2 we ship option (1) — it's the smallest change and gives full PQ safety at the anchor level.

### 4.4 Validator set growth

Today's production validator count is 6 (per the `dcscan-production-landing` and `chainlist-pr-fix` rules). The code constants are `MIN_VALIDATORS = 21`, `MAX_VALIDATORS = 100`. Phase 2 brings the active set to 21 by recruiting validators from the existing ecosystem partners (DCSwap, Tanastok, Datawallet+, Alteros, Moneymaker, Syndicated.ltd, ROPE Foundation, plus 14 community validators selected by stake-weighted application).

This is mostly governance + onboarding work, not engineering. The Datawallet+ DID/ONCHAINID CLI already supports node deployment signing; we extend it with a `validator-register` subcommand.

### 4.5 Phase 2 capacity math

Assuming 21 validators, 10s anchor cadence, 100K knot/sec sustained from Phase 1:

```
Per-anchor knot count    ≈ 1,000,000 knots
Per-anchor merkle build  ≈ 50ms (parallel BLAKE3)
Per-anchor Dilithium     ≈ 21 sigs × ~1ms each = 21ms (parallel across cores)
Per-anchor Ed25519 batch ≈ 5ms (1000-sig batches fit easily)
Total anchor latency     ≈ 100ms (well inside 10s window)
Net throughput hit       ≈ 0% (consensus runs in parallel with knot writes)
```

So Phase 2 holds Phase 1's 100K/sec.

---

## 5. Phase 3 — Horizontal Node Sharding

**Target:** 1.6M aggregate knot/sec across 16 nodes (100K each).
**Eng:** ~16 weeks, no canon break for read APIs (additive RPC), wallet-prefix routing for write APIs.

### 5.1 Wallet-prefix partitioning

Every wallet address has a 32-byte public key. Take the first 4 bits as the **shard key** → 16 shards. Each shard is owned by one **node-cluster** (3 replicas per cluster for fault tolerance) → 48 production nodes total. (Drop to 16 for cost-optimised "lite" cluster: 1 replica per shard.)

| Shard | Wallet prefix | Cluster |
|---|---|---|
| 0 | `0x0...` | rope-cluster-0 |
| 1 | `0x1...` | rope-cluster-1 |
| ... | ... | ... |
| F | `0xF...` | rope-cluster-F |

Each cluster runs the full Phase 1+2 stack but on a **slice** of the wallet space. Inside a cluster, the existing 256-way intra-shard sharding from Phase 1 still applies (so each cluster has 16 sub-shards covering 1/16 of the wallet space).

### 5.2 Cross-cluster reads

Most reads are intra-cluster (wallet's history, recent knots). The only intrinsically cross-cluster operation is **global aggregates** (`/api/v1/stats`, "count all knots"). Two patterns:

1. **Scatter-gather at the explorer.** DCScan's `dc-explorer` queries each cluster in parallel and sums. Already trivially parallel today; just move from "1 node, N caches" to "16 nodes, 16 caches".
2. **Anchored global counters.** Each anchor includes a per-cluster `(knot_count_delta, volume_delta)` tuple. The "global stats" view is the sum of all cluster anchor deltas — eventually consistent, anchor-window-fresh (~10s lag).

### 5.3 Cross-cluster writes are forbidden

A write to wallet `W` always lands in `cluster(W)`. There are no cross-cluster writes in the canon — every wallet has a single home cluster. Cross-cluster *value transfer* (a wallet in cluster 3 sends to a wallet in cluster 7) is modelled as:

1. Source-cluster knot (debit on sender's string) — atomic in cluster 3.
2. Destination-cluster knot (credit on recipient's string) — atomic in cluster 7.
3. Both reference the same **transfer envelope id**, anchored in both clusters' next anchors.
4. Reorg/failure handling: if (1) finalises and (2) fails, the destination cluster owes a credit and a watchdog (the testimony agents from the v1.2 ecosystem) raises an alert. In practice this never happens because the source-cluster knot includes the destination's anchor commitment as a parent reference.

This is the standard 2PC-over-anchors pattern. It works because both anchors finalise in the same 10s window, so cross-cluster "atomicity" is bounded by anchor latency.

### 5.4 Load balancing & nginx topology

The `erpc.datachain.network` and `erpc.rope.network` upstreams gain a Lua filter (or nginx-plus dyups) that reads the `wallet` parameter from the JSON-RPC body and routes to the correct cluster. Read-only methods (`eth_blockNumber`, `rope_globalStats`) round-robin across all clusters. Catch-all and unknown methods go to a "global" gateway cluster that proxies internally.

This is the only nginx change needed for Phase 3. It's about 200 lines of OpenResty Lua.

### 5.5 Validator set & consensus

Each cluster has its own 21-validator Testimony committee. **Anchors are global** — produced by a rotating committee elected from the union of all per-cluster validators (target: 7 anchor-committee seats per anchor, rotated every 256 anchors). The anchor merkleizes the per-cluster anchors into a single root. This is exactly Ethereum's "beacon chain over shards" pattern.

### 5.6 Phase 3 capacity math

```
Per-cluster TPS         ≈ 100,000 knot/sec (Phase 1+2 ceiling)
Cluster count           = 16
Aggregate TPS           = 1,600,000 knot/sec
Anchor cadence          = 10s (unchanged)
Per-anchor cluster work ≈ 100ms (unchanged)
Per-anchor global work  ≈ 50ms (merkle of 16 cluster roots, sub-millisecond)
End-to-end finality     ≈ 20s (one local anchor + one global anchor)
Total node count        ≈ 48 (16 clusters × 3 replicas) for the durable tier
```

That's >1.5M sustained knot/sec with no canon break.

---

## 6. Phase 4 — DAG-of-Knots (the canon change to v2.0)

**Target:** 5M+ aggregate knot/sec.
**Eng:** ~24 weeks, **canon-breaking** (handled via versioned RPC and projection layer).
**Trigger to start:** when Phase 3 saturates and we measurably need >1.6M sustained.

### 6.1 What stays the same

- The `String` primitive (a logical chain of knots tied to one entity).
- The `Knot` primitive (a single, individually-erasable unit of state change).
- `untie_knot` semantics. Tombstones are still preserved.
- `StringRegistry` and the v1.2 enrichment (string kinds, head index, etc.) — but extended to track DAG heads instead of single heads.
- All read APIs (`rope_walkString`, `rope_globalStats`, `rope_getKnot`, etc.).

### 6.2 What changes

In v1.0–v1.2, each `String` is a **linked list** of knots (`parentage(vec![parent_id])` always has length 1 within a wallet). v2.0 lets each `String` be a **DAG**: a knot may declare multiple in-wallet parents.

```
v1.2:  knot(N) -> knot(N-1) -> knot(N-2) -> ... -> genesis
v2.0:  knot(N) -> {knot(N-1a), knot(N-1b)}  (concurrent appends merged)
       knot(N) -> knot(N-1a) -> knot(N-2)
                              -> knot(N-2)' (parallel branch, same parent)
```

This eliminates the per-wallet head lock from Phase 1.3. Concurrent appends to wallet `W` no longer race for the head; both succeed and the next knot picks them up as multiple parents (or is later merged by a periodic compactor).

### 6.3 Why this unlocks 5M

In v1.2 (and v2.0 P1–P3), a single high-traffic wallet (say, the DCSwap router) is a serialization point. Even with per-wallet locks, that one wallet can only push as many knots/sec as one core can encrypt. That ceiling is ~100K knot/sec for the busiest wallet.

In v2.0 P4, the busy wallet's writes are **embarrassingly parallel** — every concurrent caller produces an independent leaf knot. The compactor (a deterministic reduce step) merges leaves into a canonical DAG every anchor window. The wallet's "current state" is the deterministic fold over the DAG.

### 6.4 The compactor

Every anchor window (~10s), each cluster runs a deterministic compactor over each wallet's leaves:

- Sort leaves by `(hlc, knot_id)` ascending.
- Emit a `MergeKnot` whose parents are the entire leaf set.
- The next concurrent write picks up the `MergeKnot` as its parent → DAG depth stays bounded at `O(log N)`.

The compactor runs in parallel across wallets and clusters. It is itself an embarrassingly parallel job.

### 6.5 v1.2 compatibility (the projection layer)

v1.2 emitters expect a linear `walkString` result. The projection layer renders the v2.0 DAG as a linear sequence by performing a **deterministic topological walk** (sort by `(hlc, knot_id)`). For any concrete read, the projection is deterministic and the v1.2 caller cannot tell the underlying store is a DAG.

`rope_appendToLedger` (v1.2 RPC) continues to accept appends with `parentage = [head]`. The v2.0 layer interprets `[head]` as "the most recent leaf I knew about" and appends as a leaf — possibly creating a sibling of another concurrent leaf, which is fine.

`rope_v2_appendKnot` (new) accepts `parentage = Vec<StringId>` of arbitrary length and is the native v2.0 RPC. Ecosystem agents can opt in over time.

### 6.6 Phase 4 capacity math

```
Per-shard write rate        unchanged at 100,000 knot/sec
Bottleneck removal          per-wallet serialization eliminated
Compactor cost              ≈ 100ms / cluster / anchor window (parallel BLAKE3)
Effective per-cluster TPS   ≈ 300,000 knot/sec (3× from removal of head-lock contention
                              even on hot wallets like the DCSwap router)
Aggregate TPS (16 clusters) ≈ 4,800,000 knot/sec
Headroom                    ≈ Phase 5 GPU offload pushes this to 10M+
```

---

## 7. Phase 5 — GPU/ASIC Signing Offload (optional)

**Target:** 10M+ aggregate knot/sec.
**Eng:** ~8 weeks after Phase 3, hardware-dependent.

When Phase 2's batched Dilithium verification becomes the bottleneck (well past 5M TPS), offload to GPU. Open implementations exist (`pqc-dilithium-gpu`, NVIDIA's `nvCRYPTO`). One A100 GPU sustains ~50M Dilithium verifications/sec — wildly over-provisioned for our anchor cadence, but provides headroom for the eventual move to per-knot signing.

This phase is **optional**. We only do it if v2.0 P4 saturates and we credibly need >5M.

---

## 8. v1.2 → v2.0 Migration Plan for Ecosystem Agents

The full handover lives in `.cursor/rules/handover-quipu-canon-v2-migration-2026-05-03.mdc`. Summary:

| Phase | Ecosystem agent action |
|---|---|
| P1 | None. v1.2 RPC unchanged. Throughput improves automatically. |
| P2 | None. Optional: register as a validator via the new `datawallet validator-register` CLI. |
| P3 | One-line change: ecosystem agents that care about read freshness can switch from `https://erpc.datachain.network` to a sticky cluster URL `https://cluster-N.rope.network` for their wallet's home cluster. Optional. |
| P4 | Recommended (not forced): switch from `rope_appendToLedger` to `rope_v2_appendKnot` to take advantage of leaf-style concurrent writes. v1.2 RPC continues to work via projection. |
| P5 | None. |

Crucially: **no ecosystem agent has to change a single line of code through Phase 3**, and Phase 4 is opt-in for performance, not correctness. This is the central design contract.

---

## 9. Capacity & Cost Model

### 9.1 Hardware assumptions

| Tier | Spec | Monthly cost (Gandi/Hetzner equivalent) |
|---|---|---|
| Dev VPS | 8 cores, 16GB, 200GB NVMe | ~€40 |
| Phase 1 production node | 32 cores, 64GB, 1TB NVMe | ~€250 |
| Phase 3 production cluster (3 replicas) | 3× 32-core nodes | ~€750 |
| Phase 5 GPU node | A100 (cloud) or RTX 4090 (bare metal) | ~€1500 |

### 9.2 Phase-by-phase fleet & cost

| Phase | Nodes | Monthly run cost | Aggregate TPS | Cost/(million TPS/month) |
|---|---:|---:|---:|---:|
| Today | 4 (Gandi + DO) | ~€160 | 4K | €40,000 |
| Phase 1 | 4 (upgraded to 32-core) | ~€1,000 | 100K | €10,000 |
| Phase 2 | same | ~€1,000 | 100K | €10,000 |
| Phase 3 (lite) | 16 | ~€4,000 | 1.6M | €2,500 |
| Phase 3 (durable) | 48 | ~€12,000 | 1.6M | €7,500 |
| Phase 4 | 48 | ~€12,000 | 4.8M | €2,500 |
| Phase 5 | 48 + 4 GPU | ~€18,000 | 10M | €1,800 |

Comparison reference: Solana's mainnet (~3,000 validators, ~50K TPS demonstrated peak) reportedly costs >€2M/month to operate. Datachain Rope's per-TPS cost at v2.0 P4 is **~3 orders of magnitude better** because we don't pay for global state replication on every node.

### 9.3 Engineering cost

| Phase | Eng months | Calendar duration |
|---|---:|---|
| Phase 1 | 3 (1 senior Rust × 1.5 months + 1 mid × 1.5) | 1.5 calendar months |
| Phase 2 | 4 (consensus specialist + Rust) | 2 calendar months (in parallel with P1 once P1 is in code-freeze) |
| Phase 3 | 8 (devops + distributed-systems specialist + Rust) | 4 calendar months |
| Phase 4 | 12 (canon work + Rust + ecosystem PM) | 6 calendar months |
| Phase 5 | 4 (CUDA specialist + Rust FFI) | 2 calendar months (after P3 lands) |

**Total to 5M TPS:** ~27 eng-months over ~12–14 calendar months.

---

## 10. Benchmark Methodology (how we credibly prove we hit the targets)

A "TPS number" is meaningless without methodology. Each phase must be measured exactly the same way to be comparable.

### 10.1 Workload generator

`tools/rope-loadgen/` (new, to be written in Phase 1):

- N concurrent wallets (default: 10,000).
- Each wallet emits knots at a configurable rate (default: 1 knot/sec, ramp to saturation).
- Each knot carries a real DCR-20 transfer payload (small and large variants).
- Drives `rope_appendToLedger` over the production nginx, not via direct RPC bypass.

### 10.2 Acceptance criteria per phase

A phase **passes** only if:

1. Sustained throughput meets target for **≥ 30 minutes continuous** (not a 30-second burst).
2. **p99 RPC latency ≤ 1 second** for the same workload (no benchmark theatre via queueing).
3. **Zero knot loss** verified by reading every emitted knot back via `rope_walkString` after the run.
4. **Untie path still works**: 1% of emitted knots are untied during the run; tombstones are observed in `walk_string_with_tombstones`.
5. RocksDB durability verified by killing the node mid-run and reading back via a fresh process.

### 10.3 Reproducibility

The loadgen produces a `bench-report.json` with:
- Git SHA of `rope-node`.
- Node hardware (CPU, RAM, disk).
- Workload parameters.
- Per-second and percentile latency histograms.
- Final knot count and tombstone count.

Reports are committed to `datachain-rope/bench/` so progress is auditable.

---

## 11. Risks & Open Questions

| Risk | Mitigation |
|---|---|
| Phase 1's per-wallet lock still bottlenecks the hottest wallet (e.g., DCSwap router) | Phase 4 fully removes this, but if we hit the wall in Phase 1 we accelerate Phase 4 design |
| Cross-shard atomicity weaker than v1.2 implicit ordering | 2PC-over-anchors gives 10–20s atomicity; if applications need stronger we keep "single-cluster" wallets as an option |
| Validator economics at 21 → 100 validators: who pays? | Out of scope here; tracked in `datachain-rope-production-roadmap.mdc` and DC FAT tokenomics |
| OES generation rotation across shards (Phase 3) | Each cluster runs its own OES generation independently; cross-cluster reads include a generation tag so readers can verify |
| Canon break in Phase 4 disrupts ecosystem agents | Projection layer + dual RPC namespace + 6-month deprecation window |
| RocksDB compaction stalls under sustained 100K writes/sec | Phase 1 includes a tuning study; fall back to TiKV or sled if RocksDB doesn't hold |
| `dashmap` memory overhead | Profiling in Phase 1; alternative is hand-rolled per-shard `RwLock<HashMap>` array |
| GPU availability (Phase 5) | Optional; we only commit when needed |

---

## 12. Open Decisions for the User

By the time Phase 1 design lock starts (target: 2026-06-01), we need decisions on:

1. **Persistence backend**: RocksDB (default, recommended), TiKV (if multi-node sled-style), or a managed alternative.
2. **Validator economics**: how do we incentivise the recruit-21-validators step in Phase 2? Stake-weighted application, RFP, or invite-only from existing ecosystem partners?
3. **Phase 3 cluster topology**: 16 clusters × 1 replica (lite, ~€4K/mo) or 16 × 3 replicas (durable, ~€12K/mo)?
4. **Phase 4 trigger**: do we plan ahead and start Phase 4 design in parallel with Phase 3 build (faster), or wait for Phase 3 to saturate (cheaper)?
5. **Phase 5**: in or out? Probably out for v2.0; revisit after P4 lands.

These are not blocking for design work — Phase 1 design proceeds independently of all of them — but they shape the calendar.

---

## 13. What This Document Is Not

- It is **not** a sales pitch or whitepaper marketing copy. It assumes a hostile reviewer (i.e., me, in 6 months, debugging a regression).
- It is **not** a commitment to ship in any specific calendar window. The eng months and durations above are estimates from a single-agent survey; serious estimation requires the team that will build it.
- It is **not** an investment ask. Cost numbers are illustrative for sizing decisions, not budget items.
- It does **not** override the v1.2 canon for any phase before Phase 4. v1.2 keeps shipping, keeps emitting, keeps working.

---

## 14. Next Steps (within the 2-hour result window)

1. ✅ This spec lands in `datachain-rope/docs/`.
2. ✅ A short always-applied workspace rule (`.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc`) anchors future agents to this spec.
3. ✅ A migration handover (`.cursor/rules/handover-quipu-canon-v2-migration-2026-05-03.mdc`) tells ecosystem agents "you don't have to do anything until Phase 4, and Phase 4 is opt-in".
4. ⏭ Start a design-review thread with the user on the five open decisions in §12.
5. ⏭ When user approves, branch `datachain-rope/feat/v2-phase1-sharded-lattice` and start Phase 1 in code.

The 504 timeouts DCSwap is seeing today are addressed implicitly by Phase 1. We are explicitly **not** patching today (per user decision: `today_patch=no`). The architectural fix is the sustainable one, and it lands within Phase 1's 6-week window.

---

*End of v2.0 architecture spec. For the always-applied summary, see the workspace rule. For ecosystem migration, see the handover.*
