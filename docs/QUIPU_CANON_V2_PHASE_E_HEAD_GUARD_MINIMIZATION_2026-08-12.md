# Quipu Canon v2.0 - Phase E: head_guard critical-section minimization

**Status:** DESIGN (2026-08-12)
**Owner:** rope-node
**Predecessors:** Phase 1.1-1.6, Phase C (OES-outside-head-lock), Phase 2.B (parallel RocksDB writer)
**Successor:** Phase F (real consensus + validator set expansion, per `quipu-canon-v2-roadmap-5m-tps.mdc`)

---

## 1. Motivation - what P2B did NOT close

Phase 2.B (2026-08-12, deployed fleet-wide in §28 of `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc`) replaced the single-flusher persistence layer with an 8-way sharded writer pool. On BLUE post-P2B (24 h observation window in progress at time of writing):

| Metric | Pre-P2B | Post-P2B | Interpretation |
|---|---|---|---|
| `head_guard_hold.max_ns` | ~5.07 ms | ~20 ms observed (`sample-20260812T140633Z.json` + later probes) | Higher, not lower. See §2. |
| `head_guard_hold.mean_ns` | ~1.18 ms | ~596 μs -> ~1 ms | Improved on mean; tail unchanged. |
| `flusher_wait.count` | not reachable (single flusher wedged) | **0** across all observations | 8-way shard pool absorbs everything. |
| HA restart cadence (BLUE) | ~6-9 min | still ~6-9 min | Wedge cycle NOT eliminated. |

**Conclusion:** the bottleneck is NOT flusher back-pressure any more. It is now demonstrably **inside** `head_guard` on the append hot path. P2B moved the wall, but the append path still spends up to 20 ms holding the per-wallet head lock, which under bursty load (Googlebot + canonical AI agent writes) is long enough for the `erpc-fleet-ha.sh` loopback probe to time out at its 5 s budget after 3 consecutive fails.

Phase E is the design that closes the last critical-section width.

---

## 2. Root cause - what still runs inside `head_guard`

Read from `crates/rope-node/src/ledger_manager.rs::append_to_ledger` (lines ~880-982, post-Phase-C):

```
lock head_guard (per wallet, sharded)
  1. registry.get_descriptor(wallet)         -- DashMap read, <10 us
  2. oes.generation() re-read                -- atomic load
  3. optional inline OES derive (rare)       -- ~30-50 us on miss
  4. encrypt_ledger_content()                -- ChaCha20-Poly1305 + BLAKE3, ~100 us
  5. oes.generate_proof()                    -- ~5 us
  6. clock.tick_for_wallet(wallet)           -- per-wallet atomic
  7. build_append_string()                   -- Ed25519 sign ~50 us, hash ~10 us
  8. bincode::serialize(&new_string)         -- ~50 us
  9. lattice.add_string(new_string)          -- 1-15 ms, see §2.1
 10. store.put_string_blob(id, blob)         -- P2B enqueue, ~5-100 us (mutex+try_send)
 11. slice_encrypted_content()               -- ~100-500 us
 12. registry.record_append(wallet, id, ...) -- DashMap write, <20 us
 13. store.append_to_chain(wallet, id)       -- P2B enqueue
 14. store.get_descriptor(wallet)            -- DashMap read
 15. store.put_descriptor(wallet, stored)    -- P2B enqueue
 16. lifecycle.record_append(...)            -- fast metric
drop(head_guard)
```

Sum of steady-state costs: ~1-2 ms typical, ~15-20 ms under contention. Matches observed `head_guard_hold.max_ns = 20 ms`.

### 2.1 The 15 ms in `lattice.add_string`

`crates/rope-core/src/lattice.rs::add_string` (verified in Phase 1.6.β):

- Parentage verification: DashMap lookups on N shards (`parents.iter().for_each(|p| ...)`) - fast (~10 us)
- **`Complement::generate(&string)`**: BLAKE3 + serialization CPU work, ~500 us to 5 ms depending on payload size
- Insertions into 4-5 sharded structures (`complements`, `parents`, `pending`, `strings`) - each ~10 us on uncontended DashMap
- **`pending.write()` RwLock**: BTreeMap insert; the RwLock is per-shard, so this is per-shard contention (up to ~1 ms if same shard is hot)
- Parent shard `children.entry().or_default().push()` - fast
- Creator index write lock (`creator_index[shard].write()`) - per-shard, ~10 us
- `anchor_candidates[id_shard_idx].lock().push(id)` - **per-shard mutex; the P1.6.β fix that sharded this from global -> per-shard was the biggest single-op improvement, but it's still ~10 us + potential lock convoy under sustained same-shard writes**
- `schedule_maintenance()` - `OnceCell::get()` -> `try_send(())` on channel, ~10 us

**Two clear P2 candidates:**
1. `Complement::generate` (500 us - 5 ms) is pure CPU and depends only on the input string (not on wallet head or lattice state). It CAN be moved to run BEFORE the head_guard acquisition.
2. `bincode::serialize(&new_string)` (~50 us) similarly depends only on `new_string`, which is constructed from `head_id + sequence_number + user payload`. **It CAN be moved to after `add_string` returns** (we already have the string data), and even further, into a background task.

---

## 3. Phase E scope (3 sub-phases)

### Phase E.1 - Pre-compute and post-compute (~1 week eng)

**Goal:** shave 500 us - 5 ms off `head_guard_hold` typical, up to 10 ms off tail.

Move outside the head_guard:

| Op | Direction | Correctness constraint |
|---|---|---|
| `Complement::generate` | POST head_guard - after `add_string` returns | Complement is a derived commitment over the string; storing it is not on the write-path critical section. Move `shard.complements.insert(id, complement)` to a background task or defer to first-reader lookup. |
| `bincode::serialize(&new_string)` | POST head_guard, before `put_string_blob` | The blob is only needed for persistence; the in-memory lattice already has the string. Serialize AFTER `add_string` returns the id. |
| `slice_encrypted_content()` | POST head_guard | Only used for `piece_count` metric downstream. Recompute in `lifecycle.record_append()` or persist as a lazy field. |
| Pending push (`pending.write()`) | Defer to `schedule_maintenance()` | Move the BTreeMap insert into the maintenance actor's tick, not the write path. The write path only needs the string to be visible in `shard.strings` (which happens last, correctly). |
| `oes.generate_proof()` | PRE head_guard (with fallback) | Same pattern as Phase C for OES key derivation: pre-compute assuming `generation` is stable; if it rotated during lock acquisition (rare), regenerate inside. Only ~5 us but demonstrates the pattern. |

**Correctness invariants preserved:**
- Per-wallet append ordering: `head_id + sequence_number` still read+written atomically inside head_guard. Unchanged.
- Cross-wallet parallelism: improved (less time under head_guard).
- Reader consistency: `shard.strings` still published LAST inside `add_string`, so a concurrent walker never sees a string without its parents/complement being findable.
- Crash recovery: `put_string_blob` still enqueue-then-mirror. If we crash after `add_string` succeeds but before `put_string_blob` enqueue, the in-memory lattice has the string but the persistence layer does not - on reboot, lazy rehydration (§12) restores from persistence, so the string is silently forgotten. This matches current behavior (Phase 2.B). No worse.

### Phase E.2 - Batched enqueue (~1 week eng)

**Goal:** replace the current three-enqueue-per-append pattern (put_string_blob, append_to_chain, put_descriptor) with a single batched enqueue.

**Current cost per append:** 3 * (mutex lock + try_send + mutex unlock) = ~15-30 us just in enqueue overhead. Under 8 concurrent appends hitting the same shard's tx mutex, this can convoy up to ~200 us.

**Design:**

```rust
// New WriteOp variant in rocksdb_persistence_p2b.rs
enum WriteOp {
    // ... existing variants ...
    AppendBatch {
        wallet: Vec<u8>,
        string_id: [u8; 32],
        blob: Vec<u8>,
        seq_in_wallet: u64,
        desc: StoredLedgerDescriptor,
    },
}

// In LedgerManager::append_to_ledger, replace the three enqueues with:
let seq = self.store.append_batch(
    &wallet_bytes,
    *new_id.as_bytes(),
    knot_blob,
    seq_in_wallet,
    stored,
)?;
```

The flusher on the shard's thread demux the batch into three RocksDB PUTs in a single `WriteBatch`, so we go from 3 RocksDB syscalls to 1, AND from 3 tx mutex acquisitions to 1. This is BOTH a critical-section width improvement AND a flusher throughput improvement.

**Correctness invariants preserved:**
- Cross-shard atomicity: unchanged (§22.4) - a `WriteOp::AppendBatch` still lives on one shard, so the 3 sub-puts are atomic to each other via RocksDB's `WriteBatch` semantics.
- Bounded queue: unchanged - one enqueue slot per append instead of three, so effective queue capacity is 3x better under back-pressure.
- Per-wallet ordering: unchanged - `wallet_append_counter` still assigns `seq_in_wallet` monotonically per wallet, and all writes for that wallet route to the same shard by `partition_byte_of`.
- Cross-shard atomicity for string blob vs descriptor: the string blob and descriptor go to the same shard (both partitioned on `wallet[0]`), so they are already atomic to each other. Unchanged.

### Phase E.3 - Lock-free anchor queue (optional, ~3 days eng)

**Goal:** replace `anchor_candidates[shard]: Mutex<Vec<StringId>>` with a lock-free MPSC queue or `Arc<crossbeam::queue::SegQueue>`.

**Motivation:** even after the P1.6.β sharding, the per-shard mutex is the last non-atomic operation inside `add_string`. Under sustained same-shard writes (e.g. Googlebot scan filtering to canonical agent wallets that all hash to shard 3), the mutex convoy can add 100-500 us to `head_guard_hold`.

**Design:** use `crossbeam-queue::SegQueue<StringId>` (already a workspace dep, or add). Push is lock-free CAS. Drain by maintenance actor via repeated `pop()` calls until empty.

**Deferred to Phase E.3 because:** the improvement is marginal (~5% of head_guard tail) vs the eng cost (crossbeam version pinning, dependency audit, test rewrite). Ship if Phase E.1 + E.2 don't fully close the wedge cycle; skip otherwise.

---

## 4. Test plan

### 4.1 Unit tests (per phase, added to `rope-node --lib`)

Phase E.1:
- `head_guard_hold_decreases_after_complement_moved_out` - microbenchmark before/after, assert p50/p99 both drop
- `pre_computed_oes_proof_is_correct_after_rotation` - simulate mid-lock OES rotation, assert fallback path used and proof valid
- `deferred_complement_is_findable_by_subsequent_walker` - append + immediate walk, assert complement resolved lazily is identical to eager one

Phase E.2:
- `append_batch_atomicity` - crash simulator: kill process between `WriteOp::AppendBatch` enqueue and flush, assert on reboot either all 3 sub-puts land or none do
- `append_batch_ordering` - concurrent appends to same wallet, assert `seq_in_wallet` monotonic in RocksDB
- `append_batch_queue_capacity_bounds` - fill shard queue with `AppendBatch`, assert next `enqueue()` returns `QueueFull` at documented cap

Phase E.3 (if shipped):
- `anchor_candidates_lock_free_queue_ordering` - concurrent pushes, assert all N string_ids observed by drainer
- `anchor_candidates_drain_all_shards_still_works` - post-refactor, sweep all shards, assert every appended id anchored

### 4.2 Load tests (rope-loadgen, on rope-vps loopback)

Reuse `tools/rope-loadgen/` (from Phase 1 spec). Test matrix:

| Scenario | Baseline | Phase E.1 target | Phase E.2 target |
|---|---|---|---|
| Single wallet, 1000 seq appends | 20 s | 15 s | 12 s |
| 100 wallets, 100 appends each (parallel) | 45 s | 25 s | 15 s |
| Same-shard hot spot (10 wallets all shard 3) | 50 s | 40 s | 25 s |
| `head_guard_hold.max_ns` under 100-append burst | 20 ms | 5 ms | 2 ms |
| `flusher_wait.count` under 100-append burst | 0 | 0 | 0 |

### 4.3 Crash recovery tests

Fault injection at each of these points, then reboot and verify state:

- Between `lattice.add_string` and `bincode::serialize` (Phase E.1 deferred serialize)
- Between `WriteOp::AppendBatch` enqueue and flush (Phase E.2 batch atomicity)
- Between `Complement::generate` deferral and background insertion (Phase E.1 deferred complement)

Recovery invariants (must all hold):
- Wallet's `head_id` in the persistent descriptor equals the last successfully-appended `string_id`
- Wallet's `entry_count` in the descriptor equals the length of the chain when walked
- No orphan strings (a string with no descriptor pointing at it AND no children pointing at it as parent)
- No orphan chain entries (a chain entry pointing at a `string_id` with no corresponding string blob)

### 4.4 Ledger invariant probes (per §"Nightly canon CI")

Continue existing `.github/workflows/nightly-invariant.yml` probe:
- `rope_globalStats.invariant_holds == true`
- `count(strings) <= count(knots)` (v1.2 registry invariant)
- No tombstone knot has a payload (rope_untieKnot semantics)

---

## 5. Risk register

| Risk | Severity | Mitigation |
|---|---|---|
| Deferred complement generation observed by walker before it lands | Medium | Complement lookup already has a "compute-on-demand" fallback in `Complement::from_string` for missing entries. Add explicit test. |
| `WriteOp::AppendBatch` doubles the max blob size per RocksDB `WriteBatch` and hits RocksDB's `max_write_buffer_size` cliff earlier | Low | Empirical measurement in load test; if hit, split back into 2 batches (blob alone, then chain+descriptor). |
| Lock-free queue in E.3 has a bug that drops anchor candidates | High but deferred | Comprehensive test coverage; ship only if E.1+E.2 insufficient. Fallback = keep current Mutex. |
| Pre-computing OES proof with stale generation causes silent divergence | Low | Same fallback pattern as Phase C for OES key derivation, already proven in production. |
| Cross-shard partial write recovery still not atomic | Low, unchanged from §22.4 | Documented mitigation via `await_durable` + lazy rehydration. Not made worse by Phase E. |

---

## 6. Rollout plan

Same env-flag-gated additive-code pattern as Phase 2.B:

- Each phase (E.1, E.2, E.3) ships as source with an `env!("ROPE_LEDGER_E<n>")` env flag defaulting OFF.
- Enable on BLUE first via systemd drop-in (`/etc/systemd/system/datachain-rope.service.d/32-ledger-e1.conf`, etc.).
- 24 h soak on BLUE per phase; watch `rope_latticeMetrics` histograms.
- If clean, cascade to GREEN -> DO-rpc-1 -> DO-rpc-2 with 24 h soak between each.

Total eng cost: E.1 (5 days) + E.2 (5 days) + E.3 (3 days if needed) = **2 - 2.5 weeks calendar** at focused-single-engineer cadence.

---

## 7. Success criteria (post-E.2, before E.3 decision)

Measured on BLUE after 48 h post-E.2 soak:

| Metric | Target |
|---|---|
| `head_guard_hold.mean_ns` | <= 500 us |
| `head_guard_hold.max_ns` | <= 5 ms |
| `flusher_wait.count` under sustained load | <= 1% of `head_guard_hold.count` |
| HA restart cadence on BLUE | 0 restarts/hour under normal traffic; only fires on genuine external shocks |
| CERBER R12 pages for `failover_no_fleet_status` | 0/day under normal ops (DNS never needs to failover) |

If all 5 met, defer E.3 indefinitely. If tail is stuck > 5 ms, ship E.3.

---

## 8. Explicitly out of scope for Phase E

- Real BFT consensus with 21-validator set (that is Phase F / v2.0 roadmap Phase 2)
- Multi-writer horizontal scaling (Phase G / v2.0 roadmap Phase 3)
- DAG-of-knots additive namespace (already in code as Phase 2.E behind `rope_v2_*`)
- GPU/ASIC PQ-signing offload (v2.0 roadmap Phase 5)
- Any change to Quipu Canon primitives (knots, strings, tombstones, per-knot erasure)
- Any change to the RPC surface visible to external clients

Phase E is purely a critical-section width optimization on the existing v1.2 append path. All external observable behavior stays identical.

---

## 9. References

- Phase 1 spec: `.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc` + `datachain-rope/docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md`
- Phase 2.B (predecessor): `.cursor/rules/handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc` §22
- Phase C (predecessor): same handover §21.3
- Phase 1.6.β lattice fix (predecessor): same handover §18
- Phase 2.E DAG (parallel additive namespace): `datachain-rope-v2/docs/QUIPU_CANON_V2_PHASE2E_KNOT_DAG.md`
- Current lattice source: `crates/rope-core/src/lattice.rs`
- Current append path: `crates/rope-node/src/ledger_manager.rs::append_to_ledger`
- Current P2B backend: `crates/rope-storage/src/rocksdb_persistence_p2b.rs`

---

*Design frozen 2026-08-12. Implementation contingent on operator green-light and eng bandwidth. Not blocking; the current fleet state (post-P2B) is honestly production-viable per §21.7 of the same handover.*
