# BLUE Never-Hangdown Alternatives Menu

**Date:** 2026-08-23T10:15Z
**Author:** Datachain Rope agent
**Trigger:** Operator directive following the MTBF postmortem: "BLUE can't hangdown every 5-7 or even 30 minutes, it should never hangdown."
**Supersedes:** Nothing. This document sits alongside `MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` and expands the mitigation menu with alternatives that the postmortem only touched on.
**Reading order:** Read `MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` first (the diagnosis), then this doc (the design space), then `ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` (Option A execution).

---

## 0. TL;DR

The operator picked Option A (upgrade rope-vps 8 GB -> 16 GB RAM) as the P0 mitigation. That is correct but **necessary and not sufficient**. Live evidence gathered while drafting this doc shows:

- BLUE is currently in its 8th restart of the hour. **~8 wedges/hour, every hour, for the last 24 h.** 2,182 recovery events logged in the past 24 h. This is not a spike, this is steady state.
- BLUE runs **61 active services** on 4 cores / 8 GB RAM. Only 2 of those (`datachain-rope`, `reth-rope`) actually need to be on the sealer.
- Post-restart, `rope-node` starts at 161 MB RSS and grows to a peak of **2,086 MB** in 7 minutes before wedging again. Add `ipfs` at 1.67 GB, `semantic-agent` at 471 MB, `reth` working set, docker, nginx, cerber, and 15 other agents. The 8 GB budget was exceeded before the sealer even fully rehydrated.
- 16 GB gives ~2x headroom. That likely turns 7-min MTBF into hours or days. But `rope-node`'s memory grows with the ledger. Any solution that doesn't remove workload from the sealer or bound its memory algorithmically will hit the same wall again in months.

**The only durable answer is "no single node is critical."** Everything below is a step toward that.

## Full alternatives menu (grouped by tier)

| Tier | Alternative | Impact | Effort | Downtime | Risk |
|---|---|---|---|---|---|
| **P0** | A1 - Move ipfs + semantic-agent + non-sealer agents off BLUE | High | 1-2 days | Zero (rolling) | Low |
| **P0** | A2 - Kernel + systemd VM tuning (MemorySwapMax=0, vm.dirty_ratio=5, min_free_kbytes=512MB, swappiness=1) | Medium | 1 hour | Zero | Low |
| **P0** | A3 - VPS upgrade 8 GB -> 16 GB (Option A, already picked) | High | Operator-blocked | 10-15 min BLUE-only | Low |
| **P1** | B1 - Automated writer promote with fencing tokens | Very High | 2-3 weeks | Zero | Medium |
| **P1** | B2 - In-process memory circuit breaker in rope-node | High | 1-2 weeks | Zero | Low |
| **P1** | B3 - Dedicated sealer host (rope-vps stays for edge; new box takes sealer only) | Very High | 1 week + 1 h migration | 10 min sealer only | Low |
| **P2** | C1 - Quipu Canon v2.0 Phase 4 (DAG-of-knots multi-writer) | Ultimate | 3-4 months | Zero | Medium |
| **P2** | C2 - Bare-metal migration off Xen (Hetzner / OVH) | High | 1-2 weeks | 10 min | Low |
| **P2** | C3 - Multi-region hot standby (Paris + Frankfurt + London) | Very High | 4-6 weeks | Zero | Low |
| **anti** | Do not increase `Restart=` aggressiveness alone | - | - | - | Masks bug |
| **anti** | Do not disable ghost-reclaim | - | - | - | Silent-drop regression |
| **anti** | Do not promote GREEN as second writer without fencing | - | - | - | **Ledger fork** |

Recommended sequence: A1 -> A2 -> A3 (this week, zero code) -> B2 -> B1 (2-4 weeks) -> B3 or C2 (5-6 weeks) -> C1 or C3 (quarter).

---

## 1. Live evidence gathered 2026-08-23T10:04Z

### 1.1 Wedge cadence (24 h rolling)

```
Hour            HEAL_ISSUED events
2026-08-22T11   9
2026-08-22T12   7
2026-08-22T13   9
2026-08-22T14   7
...
2026-08-23T09   6
2026-08-23T10   1 (in progress at capture time)

Total 24 h:     ~180 hourly-rolled restarts (2,182 raw events with retries)
Mean MTBF:      ~7.5 minutes
Std deviation:  ~1 minute (very tight; steady-state failure mode)
```

Not a spike. Not a diurnal pattern. Not correlated with peer probe traffic. This is a memory-growth cycle: BLUE restarts, rope-node rehydrates from cold, memory grows for 6-8 minutes, hits the swap wall, wedges, watchdog restarts. Repeat.

### 1.2 Workload inventory on BLUE (top by RSS)

| Process | RSS | Belongs on sealer? |
|---|---:|---|
| `rope` (rope-node) | 4,309 MB (VmPeak 2,086 MB in a fresh restart) | Yes (essential) |
| `ipfs daemon` | 1,667 MB | **No - move to dedicated ipfs box** |
| `semantic-agent` | 471 MB | **No - move to attester** |
| `reth` | 176 MB active / 260 MB swapped | Yes (essential) |
| `cerber-mesh serve` (two instances) | 138 MB + 132 MB | Marginal - useful but not essential |
| `crowdsec` | 51 MB | Yes (security) |
| `dc-explorer` | 38 MB | **No - move to a public edge box** |
| `dockerd` + `containerd` | 45 MB | Marginal |
| `token-publisher` | 22 MB | **No - move off** |
| 12 other agents (oracle, insurance, compliance, rope-idp, rope-edc, rope-evm-attester, rope-evm-proposer, rope-ecosystem-discovery, ...) | ~50 MB combined | **No - move to attester** |

Immediate offload potential: **~2.1 GB RSS freed** by moving `ipfs` and `semantic-agent` off BLUE, plus **fewer context switches, fewer syscalls, lower CPU steal, lower disk I/O contention** on the sealer path.

### 1.3 Kernel + systemd baseline

| Setting | Current | Recommended for sealer |
|---|---|---|
| `vm.swappiness` | 10 | 1 |
| `vm.overcommit_memory` | 0 (heuristic) | 2 (strict, with computed ratio) |
| `vm.dirty_ratio` | 20 | 5 |
| `vm.dirty_background_ratio` | 10 | 2 |
| `vm.min_free_kbytes` | 67,584 (67 MB) | 524,288 (512 MB) |
| `datachain-rope.service` `MemorySwapMax` | unset | `0` (disallow swap for the sealer) |
| `datachain-rope.service` `MemoryHigh` | unset | `~11G` (soft cap on a 16 GB box after A3) |
| `datachain-rope.service` `TimeoutStopSec` | 90s | 60s |

Rationale: on a 4-core, memory-constrained VM, once the sealer starts swapping every read is a major page fault (`pgmajfault`). Setting `MemorySwapMax=0` forces the OOM killer to take the sealer down cleanly (which the watchdog + systemd restart handles) instead of letting it live in a wedged half-swapped state where it accepts TCP but never answers `eth_blockNumber`.

### 1.4 GREEN and DO-1 spare capacity (offload targets)

| Host | Cores | RAM | Used | Free capacity | Ready to absorb |
|---|---:|---:|---:|---:|---|
| GREEN (anvil-vps 92.243.25.119) | ? | 8 GB (same class) | Not measured (SSH hop timed out from workstation, but RPC is 200 OK from BLUE at 1.5 ms) | Similar to BLUE minus the extra agents | ipfs mirror + semantic-agent |
| DO-1 (157.230.18.45) | 2 | 3.8 GB | 1.6 GB | ~1.9 GB | Small agents (compliance, oracle, insurance), NOT ipfs |
| DO-2 (167.172.106.174) | ? | ? | ? | ? | Same class as DO-1 |

DO-1 is the smallest box in the fleet. Do not put ipfs there. GREEN is the natural home for ipfs + semantic-agent because it already runs the same rope-node binary as a follower.

---

## 2. Why BLUE hangs (three concurrent failure modes)

The MTBF postmortem correctly identified memory pressure / swap thrash as the dominant root cause. But three modes are contributing simultaneously. Any durable fix has to address at least two of them.

### 2.1 Memory growth in rope-node ledger caches

On restart, rope-node begins at 160 MB RSS. Within 7 minutes it reaches 2.0-2.1 GB. The growth is driven by:

1. **Ledger rehydration.** `LedgerManager::rehydrate` scans the RocksDB ledger to warm the in-memory shard maps. This is essentially unbounded: as more strings and knots accumulate, rehydration reads more state.
2. **RocksDB block cache.** Default settings let RocksDB grow its cache to fill available RAM.
3. **Testimony pool + mempool.** Non-bounded in the current implementation; grows with pending write volume.

Fix: bounded LRU caches + paged rehydration (postmortem Option D) + explicit RocksDB `block_cache_size` (postmortem Option E). This is a `rope-node` code change, medium effort.

### 2.2 Workload sprawl on the sealer box

BLUE runs 61 services. The sealer competes with `ipfs` (1.67 GB) and `semantic-agent` (471 MB) and 15+ other agents for the same 8 GB and same 4 cores. Every allocation the sealer makes fights for the same page allocator, page cache, and dirty-page writeback budget.

Fix: A1 (workload migration). No code change. This is the highest ROI action available.

### 2.3 Xen hypervisor noise (Gandi VPS)

Gandi's VPS is a Xen VM on shared hardware. Two symptoms visible in the fleet:

- **CPU steal spikes** during other tenants' bursts (visible in `vmstat` `st` column).
- **Memory ballooning.** Xen can reclaim VM memory under pressure across the whole hypervisor. When the hypervisor is contended, the guest sees fewer effective pages.
- **Disk I/O jitter.** Shared block devices show 50-200 ms tail latency spikes.

Fix: C2 (bare-metal migration). Larger effort but eliminates the whole class of neighbor-effect issues.

---

## 3. P0 - do this week (no code, no downtime)

### 3.1 A1 - Move workload off BLUE

**Target state:** BLUE runs only `datachain-rope`, `reth-rope`, `nginx`, `dc-explorer` (read-only), `cerber-mesh` (single instance), `crowdsec`, `fail2ban`, `endlessh`. Everything else moves.

**Sub-steps:**

1. **ipfs -> GREEN or a new small DigitalOcean droplet (`rope-ipfs-1`).**
   - Copy IPFS repo to new host (`rsync -a /var/lib/ipfs/ new-host:/var/lib/ipfs/`).
   - Bring up `ipfs` on new host, verify peers connect.
   - Add new host to nginx `upstream ipfs_gateway` on BLUE.
   - Repoint external DNS for `ipfs.datachain.network` to the new host.
   - Stop `ipfs` on BLUE, disable service.
   - Frees 1.67 GB RSS.

2. **semantic-agent -> GREEN.**
   - Semantic index is derived state; can be rebuilt.
   - Copy tantivy index to GREEN, start `semantic-agent` there.
   - Add reverse proxy from BLUE to GREEN for `semantic-agent.datachain.network`.
   - Stop `semantic-agent` on BLUE, disable service.
   - Frees 471 MB RSS.

3. **compliance-agent + insurance-agent + oracle-agent -> DO-1 or DO-2.**
   - Small footprint each. Move together to one attester.
   - No user-facing endpoint moves.
   - Frees ~50 MB RSS.

4. **token-publisher -> anywhere.** 22 MB. Move to any attester.

5. **rope-idp + rope-edc + rope-evm-attester + rope-evm-proposer + rope-ecosystem-discovery -> DO-1.**
   - Combined ~15 MB. Move together.

**Total freed: ~2.1 GB RSS + significant CPU / I/O contention relief.**

**No downtime.** Each move is: start on new host, cut over DNS or nginx upstream, stop on old host.

**Blast radius:** if new host fails, the moved service fails, not the sealer. GREEN and DO-1 already run their own follower nodes and can host these agents. Do not move ipfs to DO-1 (too small).

### 3.2 A2 - Kernel + systemd tuning

Add to `/etc/sysctl.d/99-rope-sealer.conf`:

```
vm.swappiness = 1
vm.overcommit_memory = 2
vm.overcommit_ratio = 80
vm.dirty_ratio = 5
vm.dirty_background_ratio = 2
vm.min_free_kbytes = 524288
```

Apply: `sudo sysctl --system`.

Add to `/etc/systemd/system/datachain-rope.service.d/99-memory.conf`:

```
[Service]
MemorySwapMax=0
MemoryHigh=7G
```

(On a 16 GB box after Option A, raise `MemoryHigh` to `13G`.)

Apply: `sudo systemctl daemon-reload && sudo systemctl restart datachain-rope.service`.

**Effect:** the sealer never swaps. Under pressure, memory allocations either succeed against `MemoryHigh` (which triggers cgroup reclaim first) or the sealer is OOM-killed cleanly. The watchdog restarts. Total downtime: ~30 s per event instead of 7-8 minutes wedged.

### 3.3 A3 - VPS upgrade (already picked, see runbook)

Runbook: `ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md`. Do this after A1 (so the upgraded box actually helps rather than being filled by the same 61-service workload).

---

## 4. P1 - do in the next 2-4 weeks (medium code, high impact)

### 4.1 B1 - Automated writer promote with fencing tokens

Today, the writer promote is manual (Writer Promote Runbook §2, ~10 min). Automating it turns a 7-min wedge into a ~30 s failover.

**Design outline:**

1. **Fencing token.** Every sealer writes a monotonic epoch counter to `rope_globalStats.writer_epoch`. Bumped by +1 on every promote. Nginx `rpc_router.js` includes the epoch in write-path routing.
2. **Health quorum.** Attesters (GREEN, DO-1, DO-2) probe BLUE at 5 s cadence. When >= 2 agree BLUE is unhealthy for > 60 s, they issue a signed `promote_request` to `cerber-rope`.
3. **Fence and promote.**
   - `cerber-rope` verifies quorum, calls nginx `POST /admin/fence-writer` (signed with founder key) to remove BLUE from `rpc_primary_only` upstream.
   - Increments writer epoch.
   - Calls nginx `POST /admin/promote-writer?target=green` to route write path to GREEN.
   - Broadcasts `WriterPromoted` on the mesh so `rope-explorer` and DCSwap CERBER see it.
4. **Ledger consistency.** Because BLUE is fenced before GREEN accepts writes, no dual-writer window. Because writes carry the epoch, any straggler write on BLUE is rejected by GREEN.
5. **Auto-recovery.** When BLUE comes back healthy, it becomes an attester until manual re-promote (do not auto-flap).

**Effort:** ~2-3 engineer-weeks. Includes signed admin endpoints, quorum protocol, mesh event, integration tests with kill-BLUE-and-verify-writes-continue scenario.

**Risk:** medium. Any bug in fencing = ledger fork. Requires careful test coverage.

**Recommended:** implement after A1 + A2 stabilize BLUE, because we want fewer promote events per day (to reduce blast radius while B1 is fresh).

### 4.2 B2 - In-process memory circuit breaker

Add a supervisor thread inside `rope-node` that:

1. **Watches** `/proc/self/status` `VmRSS` at 1 s cadence.
2. **Warns** at 70 % of `MemoryHigh` (evicts least-recently-used mempool entries, prunes stale strings from cache).
3. **Sheds load** at 85 %: returns HTTP 503 to `eth_sendRawTransaction` for 5 s to backpressure clients.
4. **Requests graceful restart** at 95 %: calls `systemctl restart --user datachain-rope` before OOM.

**Effect:** the sealer never gets into a wedged half-swapped state. Instead, under pressure it self-heals in seconds by evicting caches or requesting a graceful restart.

**Effort:** ~1-2 engineer-weeks. Rust code, unit tests, integration test with synthetic memory pressure.

**Risk:** low. All operations are within the existing rope-node process boundary.

### 4.3 B3 - Dedicated sealer host

Alternative to A1 + A3: instead of upgrading rope-vps and offloading services from it, spin up a **new sealer-only box** (`rope-sealer-1`, 16 GB, bare metal or dedicated VM) that runs only `datachain-rope` + `reth-rope` + `nginx`. Repoint `rpc_primary_only` upstream to the new box.

**rope-vps stays** but becomes the edge / ipfs / semantic-agent / explorer host. No more contention on the sealer path.

**Downtime:** ~10 min for the sealer cutover (same as Writer Promote Runbook §2, but planned).

**Effort:** ~1 engineer-week for provisioning + testing. No code change.

**Trade-off:** costs a second box (~$40/month on OVH, ~$80 on Hetzner AX41 dedicated). Buys a clean sealer with no neighbor workload for the foreseeable future.

**Recommendation:** consider B3 as an alternative to A1 if operator prefers "clean sealer" over "trim rope-vps."

---

## 5. P2 - months-scale, larger effort

### 5.1 C1 - Quipu Canon v2.0 Phase 4 (DAG-of-knots multi-writer)

The Quipu Canon v2.0 roadmap already schedules Phase 4 as the durable multi-writer solution. Removes the single-sealer constraint entirely. Any healthy node can produce knots on any string. Ledger is a DAG, not a linear log. Reconciliation happens via testimony consensus.

**Effort:** 3-4 months per the existing roadmap (`quipu-canon-v2-roadmap-5m-tps.mdc`, Phase 4).

**Recommendation:** stay on the current roadmap. Phase 4 is the true end state. Everything in this document is bridging until it lands.

### 5.2 C2 - Bare-metal migration off Xen

Move BLUE from Gandi (Xen VM) to Hetzner AX41 or OVH Advance-1 (bare metal). Gains:

- No CPU steal.
- No memory ballooning.
- NVMe direct storage (5-10x lower tail latency).
- Predictable memory pressure.

Cost: ~$40/month for AX41 (48 GB RAM, 6 cores, 2x NVMe). Compare to Gandi 16 GB at ~$40/month.

**Effort:** ~1-2 weeks including provisioning, security hardening, and cutover.

**Recommendation:** consider when the operator is comfortable with a hardware-level operational profile. Not urgent while Phase 4 is on the roadmap.

### 5.3 C3 - Multi-region hot standby

Deploy sealer candidates in three regions (Paris, Frankfurt, London). Sealer role rotates based on:

- **Latency to majority of peers.** Elect the sealer closest to the peer centroid.
- **Health quorum.** Only promote a sealer with a healthy quorum vote.
- **Regional failure.** If a region drops off the internet, promote to the next region.

**Effort:** 4-6 weeks including cross-region networking (WireGuard mesh between rope nodes), latency-aware routing, and B1's fencing protocol extended to multi-region.

**Recommendation:** after Phase 4 lands, this becomes trivially easy (any node in any region can be a writer). Before Phase 4, this is B1 with three targets instead of two.

---

## 6. Anti-patterns (do NOT do)

### 6.1 Do not increase `Restart=` aggressiveness alone

Setting `RestartSec=0s` or `Restart=always` (already both true) does not fix the underlying wedge. The sealer accepts TCP but doesn't answer RPC. Systemd sees the process as alive. This is why we have the self-watchdog and ghost-reclaim.

### 6.2 Do not disable ghost-reclaim

Ghost-reclaim is a safety net: if a write gets accepted on an attester (silent-drop class from 2026-07-29), ghost-reclaim pulls it back to BLUE. Turning it off would let those writes silently disappear. Tier E already mitigated its impact with the grace window + hourly cap.

### 6.3 Do not promote GREEN as a second writer without fencing

Running BLUE and GREEN as concurrent writers = **ledger fork**. Any transaction accepted by both nodes with different orderings creates two irreconcilable chain states. This is why the single-sealer constraint exists in Quipu Canon v2.0 pre-Phase-4.

If the operator wants "any node can accept writes," the path is Phase 4 (DAG-of-knots), not dual-writer.

### 6.4 Do not remove workload from BLUE by killing rope-node dependents

Some services on BLUE (`insurance-agent`, `oracle-agent`, `compliance-agent`) call rope-node RPC via loopback. If they're moved to a different host, they need to call BLUE via the public RPC. Verify their RPC clients handle the higher latency (~30 ms vs ~0.1 ms) and the failover behaviour. Semantic-agent has this pattern documented and tested. Others may not.

### 6.5 Do not skip the acceptance criteria in the postmortem

For Option A (VPS upgrade), the postmortem defines acceptance as:
- `pgmajfault` per second trends to zero over 24 h
- Swap usage trends to zero
- `cgroup memory.pressure` `full avg60 < 1.0` sustained
- MTBF > 24 h

If A1 + A2 + A3 land and MTBF is still under 24 h, do not declare victory. Move directly to B1 or B3.

---

## 7. Recommended sequence

Given operator constraints (no regression, cautious deployment, security-cat gates):

### Week 1 (this week)
1. **A1 - Migrate ipfs to GREEN or a new droplet.** Highest ROI. Frees 1.67 GB. No code. Zero downtime.
2. **A1 - Migrate semantic-agent to GREEN.** Frees 471 MB. No code. Zero downtime.
3. **A1 - Migrate insurance-agent, oracle-agent, compliance-agent, rope-idp, rope-edc, rope-evm-*, token-publisher to DO-1.** Frees ~50 MB but hugely reduces context switch pressure.

### Week 2
4. **A2 - Apply kernel + systemd tuning.** 1 hour. Zero downtime.
5. **A3 - VPS upgrade 8 GB -> 16 GB.** Follow the runbook. 10-15 min downtime on BLUE (reads keep working via failover).
6. **Observe 48 h.** MTBF target > 24 h. If met, proceed to weeks 3-4. If not, go to B3.

### Weeks 3-4
7. **B2 - In-process memory circuit breaker.** Ship a `rope-node` release with the LRU eviction + backpressure + graceful restart logic. Test with synthetic memory pressure.

### Weeks 5-6
8. **B1 - Automated writer promote with fencing.** Ship the signed admin endpoints + quorum protocol + mesh event.

### Month 3+
9. **Return to Phase 4 roadmap.** The Quipu Canon v2.0 Phase 4 multi-writer is the real end state.

### Optional (in parallel, if operator wants)
- **B3 - Dedicated sealer host.** Can replace A1 if operator prefers a clean sealer over trimming rope-vps.
- **C2 - Bare-metal migration.** Can replace A3 for larger long-term gains.

---

## 8. Cross-references

- `MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` - diagnosis of the root cause
- `ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` - Option A execution
- `WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` - manual writer promote (until B1 lands)
- `.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc` - Phase 4 (C1) authoritative roadmap
- `CRYPTOGRAPHIC_NODE_ONBOARDING_DESIGN_V1_2026-08-23.md` - Phase A of node onboarding (independent of this doc)
- `.cursor/rules/increment-never-reverse-progress.mdc` - deployment principle applied throughout

---

## 9. Open questions for the operator (before P0 lands)

1. **A1 IPFS destination.** GREEN (existing host, no new billing) or a new droplet `rope-ipfs-1` (clean isolation, ~$12/month)? Recommend new droplet for the CID-pinning role, keeping GREEN dedicated to sealer failover.
2. **A1 timing.** OK to migrate ipfs today (zero downtime) or wait for a maintenance window? Recommend today.
3. **B3 vs A1.** Prefer "trim rope-vps" (A1 + A3) or "spin up dedicated sealer" (B3)? Both work. B3 is slightly cleaner but costs one more box.
4. **C2 later.** After Phase 4 lands, does the operator still want bare metal? (Phase 4 makes hypervisor noise irrelevant because no single node is critical.)
5. **B1 quorum size.** For auto-promote, is a 2-of-3 attester quorum sufficient (GREEN + DO-1 + DO-2) or should the founder key sign the promote as a fourth gate? Recommend founder key signature required (matches existing V11 security posture).

---

*This document is the response to the operator's directive "BLUE should never hangdown." The truthful answer is: no single-writer architecture can guarantee "never." What we can guarantee is "very rarely" (P0), "self-heals in seconds" (P1), and "not the only writer" (P2 / Phase 4). The recommended sequence gets each guarantee at the earliest possible point.*
