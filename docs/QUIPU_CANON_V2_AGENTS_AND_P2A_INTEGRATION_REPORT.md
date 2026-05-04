## Quipu Canon v2.0 — Agents + P2.A Integration Report

**Branch:** `feat/v2-agents-integration`
**Base:** `feat/v2-phase1-integration` (`8981d21`)
**Date:** 2026-05-04
**Author:** Datachain Foundation DDMI

---

### Scope

This integration branch lands, in one place, all of the work produced in the
parallel session that followed the Phase 1 hardening:

1. **Quipu Canon v2.0 Phase 2.A** — `LedgerLifecycleManager` sharding
   (`feat/v2-phase2-lifecycle-shard`).
2. **Five canonical AI testimony agents** as real Rust crates:
   - `oracle-agent` (`feat/agent-oracle`)
   - `validation-agent` (`feat/agent-validation`)
   - `insurance-agent` (`feat/agent-insurance`)
   - `compliance-agent` (extended; `feat/agent-compliance`)
   - `semantic-agent` (`feat/agent-semantic`)
3. **DCScan `/agents` page redesign + canonical_ai_agents() backend audit
   fields** (`feat/dcscan-agents-page-redesign`).
4. **datachain.network/docs update** for Quipu Canon v1.2 + Phase 1 hardening
   + AI agent catalogue + DCR-20 + new RPC methods
   (`feat/datachain-network-docs-update`).
5. **Deployment artefacts (systemd units, env templates, Nginx config,
   installer)** for the five agents (`feat/agents-deploy-systemd`).

### Merge sequence

| # | Branch | Strategy | Result |
|---|--------|----------|--------|
| 1 | `feat/v2-phase2-lifecycle-shard` | `--no-ff` | clean, +323 / -33 in `ledger_lifecycle.rs` |
| 2 | `feat/agent-oracle` | `--no-ff` | clean, 7 new files |
| 3 | `feat/agent-validation` | `--no-ff` after refixing branch ref to `fb4621d` (was force-pointed at compliance commit `de79b52`) | clean, 8 new files |
| 4 | `feat/agent-insurance` | `--no-ff` | clean, 11 new files (also added `insurance-agent` + `validation-agent` to root workspace members) |
| 5 | `feat/agent-compliance` | `--no-ff` with 10 add/add conflicts in `crates/compliance-agent/src/*.rs` (insurance branch had bled compliance content in during the parallel session) — resolved with `git checkout --theirs crates/compliance-agent/` | clean after resolution, all 10 files now match the canonical compliance subagent's version |
| 6 | `feat/agent-semantic` | `--no-ff` | clean, 12 new files; standalone `[workspace]` declaration removed in the workspace-registration commit below |
| 7 | (in-tree) `feat: register all 5 canonical AI agent crates as workspace members` | direct edit | adds `oracle-agent` + `semantic-agent` to root members, drops semantic's per-crate Cargo.lock |
| 8 | `feat/dcscan-agents-page-redesign` | `--no-ff` | clean, modifies `crates/rope-explorer/src/main.rs` + `static/agents.html` |
| 9 | `feat/datachain-network-docs-update` | `--no-ff` | clean, modifies `deploy/nginx/html/datachain/docs/index.html` |
| 10 | `feat/agents-deploy-systemd` | `--no-ff` | clean, adds `deploy/agents/{systemd,env,nginx,install-agent.sh,README.md}` |

### Build + test results on the integrated branch

```
$ cargo check --workspace
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1m 30s
    (no errors; pre-existing warnings only)

$ cargo build -p oracle-agent -p validation-agent -p insurance-agent \
              -p semantic-agent -p rope-compliance-agent
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 37.40s

$ cargo test -p oracle-agent -p validation-agent -p insurance-agent \
             -p semantic-agent -p rope-compliance-agent -p rope-protocols \
             --lib --bins
    insurance-agent     : 30 / 30 pass
    oracle-agent        : 33 / 33 pass
    rope-compliance     : 52 / 52 pass
    rope-protocols (P2.A): 29 / 29 pass
    semantic-agent      : 34 / 34 pass
    validation-agent    : 28 / 28 pass
    -----------------------
    Total              : 206 / 206 pass, 0 failed

$ cargo test -p rope-loadgen
    31 / 31 pass

GRAND TOTAL across new + adjacent code: 237 / 237 tests pass.
```

### P2.A throughput on the integrated branch (manager-write benchmark)

```
$ cargo build --release -p rope-loadgen   (2m 00s, clean)

$ ./target/release/rope-loadgen manager-write -s partitioned -m memory \
                                              --payload-bytes 256 --seed 42

  Threads × Ops × Wallets   throughput (work)   p99 latency
  ------------------------- ------------------- -------------
  1 × 100  ×  50               32,808 ops/s        100 µs
  2 × 200  ×  50               69,556 ops/s         74 µs
  4 × 400  × 100               25,523 ops/s      1,385 µs
  8 × 800  × 200               15,452 ops/s      3,631 µs
```

**Honest interpretation.** P2.A's lifecycle sharding *is* present in the
integrated branch (12 sharded constructs in `ledger_lifecycle.rs`, 723
total lines vs the 388 pre-P2.A). The single-thread baseline of ~32k ops/s
matches the P2.A subagent's "after" number for the same shape. The 2t × 200ops
× 50w shape also matches (~69k ops/s here vs ~72k in the subagent report).

The 4t / 8t numbers, however, are well below the subagent's headline
"113k ops/s at 8 × 800 × 200". This is **consistent with the second cliff
the P2.A subagent itself flagged**: an O(N²) `update_finality` loop in
`rope-core::lattice::StringLattice` that runs once per anchor and dominates
above ~200 ops × 50 wallets per shard. P2.A removed the lifecycle bottleneck;
the lattice cliff that lay underneath it is now the binding constraint and
is the explicit target for **P2.B**.

This integration is therefore correct on functionality, correct on the
P2.A code being landed, and correct on test coverage; it is *not* a
demonstration of the 113k headline number. That number remains achievable
only at the per-shard scale below the lattice cliff (e.g., 2t × 200ops × 50w
hits ~70k clean) — the rest of it is unlocked by P2.B.

### Pending items (not blockers for merge)

- **P2.B**: shard `StringLattice::update_finality` (or replace its O(N²) scan
  with a per-string finality watermark). This is the next work item that
  unblocks the multi-million ops/s target.
- **`canonical_ai_agents()` source-of-truth**: when the five new agent
  binaries are deployed via `deploy/agents/`, point `apiEndpoint` /
  `metricsEndpoint` / `wallet` fields at the real running services and
  replace the hardcoded fallback JSON.
- **Validation/Insurance subagent pushes**: the original feature branches
  for validation and insurance never made it to `origin` because of the
  cross-subagent ref contention. They are nonetheless in this integration
  branch by commit (`fb4621d` and `dab0c0e` respectively).

### Deployment-readiness summary per agent

| Agent | Crate compiles | Tests pass | Has CLI | Deploy unit drafted |
|-------|----------------|------------|---------|---------------------|
| OracleAgent     | yes | 33/33 | yes (`oracle-agent`)     | yes |
| ValidationAgent | yes | 28/28 | yes (`validation-agent`) | yes |
| InsuranceAgent  | yes | 30/30 | yes (`insurance-agent`)  | yes |
| ComplianceAgent | yes | 52/52 | yes (`compliance-agent`) | yes |
| SemanticAgent   | yes | 34/34 | yes (`semantic-agent`)   | yes |

All five binaries can be built with
`cargo build --release -p oracle-agent -p validation-agent -p insurance-agent
 -p semantic-agent -p rope-compliance-agent` and installed via
`deploy/agents/install-agent.sh <agent-name>`.
