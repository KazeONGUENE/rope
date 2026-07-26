# Quipu Canon v2.0 Phase 5 — PQ Signing Offload Pipeline

**Status:** DEVELOPMENT & TESTING (promoted from design stage 2026-07-06)
**Crate:** `rope-crypto` (`src/offload.rs`), wired into `rope-node`
(`consensus_orchestrator.rs`)
**Spec ancestry:** `QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` §Phase 5
(GPU/ASIC PQ-signing offload, 600K sig/s per node target)

---

## What shipped

Phase 5's software half is now real code in the **production** tree
(`datachain-rope`, not the v2 sandbox):

| Component | Where | What it does |
|---|---|---|
| `SigningBackend` trait | `rope-crypto/src/offload.rs` | Batch-oriented hardware abstraction. Contract is "sign N messages at once" so GPU/ASIC backends can amortise transfer + dispatch overhead. Object-safe: `Arc<dyn SigningBackend>` swaps at construction time. |
| `CpuPoolBackend` | same | Production backend today: rayon data-parallel hybrid signing across all cores. `preferred_batch = 4 × cores`. |
| `OffloadSigner` pipeline | same | Bounded submission queue → dedicated collector thread → adaptive batching (takes whatever is queued, up to the backend's preferred batch, zero added latency when idle) → per-request `SignTicket` completion. Backpressure is fail-visible (`QueueFull`), never silent loss. |
| `OffloadStats` | same | submitted / signed / batches / mean batch size / queue high-water / lifetime sig/s — the numbers needed to size a GPU purchase with real data. |
| Orchestrator integration | `rope-node/src/consensus_orchestrator.rs` | Every testimony signature (`attest_and_serialize`, `notarize_transaction` self-testimony) is produced through the pipeline, with inline fallback under backpressure. |
| RPC observability | `rope-node/src/rpc_server.rs` | `rope_committeeInfo` response carries a `signingPipeline` object with the live `OffloadStats`. |
| Benchmark | `rope-crypto/examples/offload_bench.rs` | `cargo run --release -p rope-crypto --example offload_bench [N]`. Verifies **every** signature it produces (no benchmark theatre). |

## Measured results (2026-07-06, 10-core Apple Silicon laptop, release build)

```
serial (hot path today)              257.06 ms      7,967 sig/s   125.5 µs/sig
cpu pool (rayon batch)               136.68 ms     14,984 sig/s    66.7 µs/sig
offload pipeline (queue+batch)        60.17 ms     34,036 sig/s    29.4 µs/sig
pipeline stats: batches=53 mean_batch=38.8 queue_high_water=2023
verification: 6,144/6,144 signatures valid
```

The pipeline beats even the raw pool because the collector overlaps
batch dispatch with queue drain (submission of batch k+1 proceeds while
batch k signs). 4.3× over the serial hot path on laptop silicon; the
production 8-core fleet nodes should see ~3–4×.

## Path to the 600K sig/s target

The 600K/node figure requires dedicated signing hardware. The pipeline
is architected so that step is **contained**:

1. Implement `SigningBackend` for the hardware (CUDA Dilithium3 kernel,
   or a signing-ASIC driver). Everything else — queue, batcher,
   tickets, stats, orchestrator wiring, RPC surface — stays identical.
2. The backend reports its own `preferred_batch()` (thousands for a
   GPU); the collector automatically forms bigger batches.
3. `OffloadStats.mean_batch_size` and `queue_high_water` from
   production traffic tell us the real arrival rate before any
   hardware is purchased.

## Test coverage

- 6 unit tests in `offload.rs`: signature validity through the
  pipeline, order preservation over 64-message batches, stats
  correctness, backpressure surfacing (capacity-1 queue), pool
  correctness, and a multi-core speedup guard.
- 5 orchestrator integration tests in `consensus_orchestrator.rs`
  exercise the full attest → wire → verify path (which now runs
  through the pipeline).
- Benchmark hard-fails if any produced signature does not verify.
