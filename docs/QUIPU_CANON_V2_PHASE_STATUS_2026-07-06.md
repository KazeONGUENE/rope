# Quipu Canon v2.0 — Phase Status Report (2026-07-06)

**Author:** Datachain Rope agent
**Baseline:** `docs/QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md` (2026-05-03 design)
**Scope:** This report records the transition of Phases 2, 4, and 5 from
*design* to *code-complete in the production tree* (`datachain-rope`, the
v1 codebase). Per the founder's directive of 2026-07-06, the v2
development tree (`datachain-rope-v2`) was used as a donor of features —
the v1 foundation was **enriched, not erased**. Every port below is
additive: no v1 module was deleted or replaced destructively.

---

## Phase status matrix

| Phase | Headline | Design (2026-05-03) | Status now | Where the code lives |
|---|---|---|---|---|
| 1 | Sharded lattice + per-wallet head lock + cached OES + RocksDB persistence | Design | **DEPLOYED** (RocksDB persistence live on fleet since 2026-07-03; benchmark: `docs/QUIPU_CANON_V2_PHASE1_BENCHMARK_RESULTS.md`) | `rope-core/src/lattice.rs`, `rope-storage` |
| 2 | Real Testimony consensus, verified hybrid signatures, validator registry, roster tooling | Design | **CODE-COMPLETE in production tree** — `verify_signatures: true` default, testimony gossip wired, `rope_committeeInfo` / `rope_validatorIdentity` / `rope_submitTestimony` / `rope_registerValidator` RPC live, `rope-cli committee` roster tooling shipped. Fleet deploy + 6→21 validator expansion pending. | `rope-consensus/src/{testimony,validator_registry}.rs`, `rope-crypto/src/batch.rs`, `rope-node/src/{consensus_orchestrator,validator_keystore}.rs`, `rope-cli` |
| 3 | Horizontal node sharding (16 wallet-prefix clusters) | Design | **OPS-STAGED** — proof fleet provisioned on DigitalOcean/Exoscale; runs the same verified-consensus binary. Full 16-cluster routing is an operations rollout, not a code gap. | fleet provisioning scripts + nginx routing |
| 4 | DAG-of-knots behind versioned `rope_v2_*` namespace (canon v2.0) | Design | **CODE-COMPLETE in production tree** — additive port, this session. See below. | `rope-core/src/knot_dag.rs`, `rope-node/src/dag_ledger.rs`, `rope-node/src/rpc_server.rs` |
| 5 | PQ-signing offload (CPU pool now, GPU/ASIC later) | Design | **CODE-COMPLETE in production tree** — offload pipeline integrated into the consensus orchestrator; measured 4.3× speedup (34,036 sig/s vs 7,967 serial). See `docs/QUIPU_CANON_V2_PHASE5_PQ_OFFLOAD.md`. | `rope-crypto/src/offload.rs` + `examples/offload_bench.rs` |

---

## Phase 4 port detail (this session)

The Phase 4 canon break ships exactly as the architecture spec §6
mandated: a **versioned, additive RPC namespace** that runs alongside the
untouched v1.2 linear ledger. No flag day, no coordinated freeze, no
change required from any v1.2 emitter (DCSwap, Tanastok, Datawallet+, …).

### What was added to v1

| Artefact | Content |
|---|---|
| `rope-core/src/knot_dag.rs` | `KnotDag` (multi-parent, per-wallet DAG with deterministic merge-free linear projection), `KnotDagRegistry` (256-shard concurrent registry), snapshot serde. 22 unit tests. v1's `knot_hash.rs` (§6.1.1 chain construction) is untouched and remains the erasure-survivable hash path. |
| `rope-node/src/dag_ledger.rs` | `DagLedger` node service: content-addressed knot ids, 256-shard payload store, per-wallet append sequences, tip compaction with threshold (default clamped ≥ 2), stats counters. 6 tests. |
| `rope-node/src/rpc_server.rs` | Five dispatch arms: `rope_v2_appendKnot` (tip-set or explicit-parent append), `rope_v2_walkString` (deterministic linear projection, v1.2-shaped), `rope_v2_tips`, `rope_v2_compact`, `rope_v2_stats`. Shared `parse_interaction_record` so v1.2 and v2.0 appends see identical payload semantics. |
| `rope-node/src/rpc_auth.rs` | `rope_v2_appendKnot` and `rope_v2_compact` added to `DESTRUCTIVE_METHODS` — same V11 public-listener gate as `rope_appendToLedger`. The list-locked CI test was extended. |
| End-to-end test | `rope_v2_dag_namespace_end_to_end` in `rpc_server.rs`: linear appends → explicit-parent fork (2 tips) → walk projection (3 knots) → compact (merge) → stats — then asserts the write methods stay `-32401`-gated for public callers while reads keep answering. |

### Canon-compliance checks

- **v1.2 invariants preserved:** `rope_appendToLedger`, `rope_walkString`,
  `rope_globalStats`, and the tombstone/`rope_untieKnot` path are
  untouched. The v1.2 namespace remains permanent per the roadmap rule.
- **Security posture preserved:** both new mutators are gated by the
  destructive-RPC layer from `SECURITY_AUDIT_2026-06-11.md`; the
  `rpc_auth_destructive_list_locked` test enforces the registration.
- **Projection contract:** `rope_v2_walkString` returns the same shape a
  v1.2 walk returns for a linear ledger, so readers built on v1.2
  semantics can consume v2.0 wallets unmodified.

---

## Test evidence (all local, 2026-07-06)

| Crate | Result |
|---|---|
| `rope-core` | 116/116 (includes 22 `knot_dag` tests) |
| `rope-consensus` | 28/28 (verified-signature testimony + registry) |
| `rope-crypto` | 51/51 (includes batch verifier + Phase 5 offload) |
| `rope-node` | 113/113 (includes `dag_ledger`, Phase 4 e2e, Phase 2 orchestrator cross-node tests, V11 gate) |
| Workspace build | `cargo build --workspace` — 0 errors |

---

## Remaining work before the phases are *production-live*

1. **Fleet deploy** via `deploy/scripts/deploy-fleet.sh` (build on GREEN
   /jammy per the glibc-skew lesson in
   `handover-security-audit-2026-06-11.mdc`), then live verification of
   testimony gossip and `rope_v2_stats` on every node.
2. **Validator expansion 6 → 21**: generate identities with
   `rope-cli committee identity` on each new witness, assemble the roster
   with `rope-cli committee build`, distribute `validator_set.json`.
3. **Ecosystem opt-in** to `rope_v2_appendKnot` per the migration
   handover — recommended, never forced (spec §8).
