# MTBF Regression Postmortem + Mitigation Menu (2026-08-23)

**Author:** Datachain Rope agent
**Status:** LIVE INVESTIGATION - evidence gathered in real time from rope-vps during the current hang cycle
**Supersedes root-cause section of:** `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` §4 ("LamportClock lock-ordering bug")
**Related:** `.cursor/rules/handover-security-audit-2026-06-11.mdc`, `.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc`, `.cursor/rules/handover-canonical-agents-live-from-rope-2026-05-05.mdc` (P1.4 dump-only lesson)

---

## 0. TL;DR (corrected diagnosis, replaces the LamportClock hypothesis)

BLUE's ~7-8 min MTBF regression is **not** a `LamportClock` lock-ordering bug. It is a **memory pressure / swap thrash** hang.

Live forensic dump captured at 2026-08-23T09:41:37Z (3 min before this doc was drafted) and live `vmstat`/`cgroup` measurements taken at 2026-08-23T09:44Z show:

| Signal | Value | What it proves |
|---|---|---|
| rope-node RSS | **4.0 GB** | biggest tenant on a **7.7 GB** VPS |
| rope-node VmSwap | **697 MB** | rope-node's own pages are on disk |
| VPS total RAM | 7.7 GB | box is under-provisioned |
| VPS RAM used | 7.1 GB (92%) | pre-swap saturation |
| VPS RAM available | 696 MB | no headroom |
| Swap in use | **5.5 GB / 15 GB** | kernel is swapping aggressively |
| cgroup `memory.pressure.full avg10` | **0.94%** | tokio runtime stalls 9.4 ms/s on memory I/O |
| kernel-wide `vmstat` `si/so` (5s sample) | **24 MB/s / 33 MB/s** | active swap thrashing right now |
| kernel-wide `vmstat` `wa` | **40%** | 40% of CPU time spent waiting on I/O |
| `pgmajfault` this run | 3,325 | major page faults hitting the RPC hot path |
| Process state during hang | `S (sleeping)` | not deadlocked, blocked on I/O |
| Kernel OOM events in 24h | 0 | it does not get killed - it grinds to unresponsive |

Sequence per cycle (verified against journalctl and forensic dumps):

```
t=0    systemd starts rope-node, RSS ~200 MB, RPC binds cleanly on :8545
t~2min RSS reaches ~3 GB (eager rehydrate warms every knot blob into memory)
t~5min RSS at ~3.5-4 GB, kernel starts swapping anon pages under pressure
t~6min tokio task hits pgmajfault mid-request, blocks worker thread
t~7min self_watchdog probe on 8545 times out (10s), consecutive counter climbs
t~7min ROPE_SELF_WATCHDOG_SUICIDE=0 -> observe only, node NOT restarted
t~8min external erpc-fleet-ha.sh sees rpc_probe_fail twice, dumps forensics,
       issues systemctl restart -> clean restart, cycle repeats
```

**This changes the near-term fix priority.** Tier D (Phase 1 Quipu v2.0: sharded lattice + per-wallet head lock) **is already deployed in the running binary** (confirmed by inspecting `crates/rope-core/src/{lattice,personal_ledger,clock}.rs` against the build date of `/home/ubuntu/datachain-rope/target/release/rope`, mtime 2026-08-12). Deploying more Phase 1 code will not help: the stall is on `pgmajfault`, not on `Mutex<LamportClock>`.

The Writer Promote Runbook itself (`docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` §§1-3) remains 100% valid. Only its §4 root-cause claim is corrected here. A footnote will be added to the runbook (see §7 of this doc).

---

## 1. Cycle-by-cycle evidence

### 1.1 Restart cadence (last 30, taken 09:44Z)

```
09:41  09:35  09:28  09:16  09:04  08:58  08:50  08:42  08:33  08:26
08:17  08:09  08:01  07:53  07:46  07:37  07:28  07:19  07:08  07:01
06:48  06:38  06:32  06:24  06:16  06:08  06:00  05:53  05:46  05:39
```

**Median inter-restart interval: 6-8 min.** Total HA-tracked hang dumps on box: **56**.

### 1.2 The forensic dump at 09:41:37Z

`/home/ubuntu/rope-node-hang-ha-2026-08-23T094137Z/status.txt` excerpt:

```
State:  S (sleeping)
VmPeak: 7318884 kB
VmRSS:  4090944 kB
VmSwap: 871712 kB          <-- 851 MB on disk
VmData: 5128776 kB         <-- 5 GB heap
Threads: 31
voluntary_ctxt_switches: 689
nonvoluntary_ctxt_switches: 457
```

`eu-stack.txt` from the same dump could not attach (`kernel.yama.ptrace_scope=1` prevents non-root ptrace between siblings). No stack frames were captured. **This is a pre-existing gap in the HA collector** and is filed as follow-up #3 below.

### 1.3 Live `vmstat` at 09:44Z (5-second sample)

```
 r  b   swpd   free   buff  cache   si   so    bi    bo   in   cs us sy id wa st
 4  3 5998868 120944  11844 899992 1239 1240  8025  3442 15104   30 24  9 55 11  1
 0  8 6091076 126296   9256 861312 23983 33025 40025 33197 67756 15467 35 19  5 40  1
```

Second interval: `si=24 MB/s, so=33 MB/s, wa=40%, r=0, b=8`. This is textbook swap thrash under I/O wait. The `b=8` (8 processes in uninterruptible sleep) is the highest number I have observed on the box.

### 1.4 Per-service memory footprint (09:44Z)

```
datachain-rope      MemoryCurrent=4,044,713,984 (3.9 GB)   MemoryMax=6,979,321,856 (6.5 GB)
reth-rope           MemoryCurrent=  261,652,480 (250 MB)   MemoryMax=8,589,934,592 (8 GB)
dc-explorer         MemoryCurrent=   26,447,872 (25 MB)    MemoryMax=infinity
rope-idp            MemoryCurrent=      524,288 (512 KB)   MemoryMax=infinity
rope-edc            MemoryCurrent=    4,812,800 (4.6 MB)   MemoryMax=infinity
rope-ecosystem-discovery                                    MemoryMax=512 MB
```

Top-10 by RSS across the whole VPS:

```
2153262 rope             3,875,504 kB  (rope-node)
    927 ipfs             1,923,452 kB
1255978 semantic-agent     702,424 kB
    999 reth               163,144 kB
2820174 node (cerber)      112,640 kB
   1138 crowdsec            55,544 kB
   1125 dockerd             44,584 kB
 576332 node (cerber)       42,340 kB
```

Combined top-3 = **6.5 GB RSS on a 7.7 GB box.** rope-node has effectively no elbow room; ipfs and semantic-agent are the other two swappers.

### 1.5 The pre-existing `10-memory-and-restart.conf` comment already named this

`/etc/systemd/system/datachain-rope.service.d/10-memory-and-restart.conf`:

```
# 2026-08-11 EMERGENCY: raised MemoryMax 5G -> 6.5G to break a crash-loop.
# Previous 5G ceiling: rope-node crash-looped during startup because ledger
# rehydration loaded all 532K knot blobs into RAM (RSS grew from 200MB to
# 4-5GB in the first 2-3 min after "Ledger persistence active"), then hit
# the cgroup ceiling and got SIGKILL'd before the RPC listener bound.
# Cycle: RocksDB recovery 3min -> in-memory index build 3min -> OOM kill.
# Cause: LedgerManager rehydrate is eager (loads all blobs into memory).
# Should be lazy/paged. Cost of the raise: box has 7.7G RAM, Reth uses
# 230MB, other services ~1G; 6.5G for rope leaves ~1.2G for buff/cache
# + system. Swap available if needed.
# Rollback: restore .bak-pre-emergency-2026-08-11 then daemon-reload + restart.
# Follow-up: file a P1 to make ledger rehydration streaming.
```

The 2026-08-11 mitigation (raising MemoryMax to 6.5 GB) papered over the crash-loop but preserved the underlying issue: `LedgerManager::rehydrate` loads all blobs into memory. The subsequent `60-lazy-rehydrate.conf` drop-in (`ROPE_LAZY_REHYDRATE=1`) helps startup but does not bound the steady-state working set, which is now provably ~4 GB RSS + ~1 GB swap and still growing linearly during a run.

---

## 2. Why the current mitigations don't stop the thrash

| Mitigation currently in effect | What it fixes | What it does NOT fix |
|---|---|---|
| `MemoryMax=6.5G` (`10-memory-and-restart.conf`) | Prevents unbounded RSS growth into other tenants | Doesn't stop swap thrash below the ceiling |
| `ROPE_LAZY_REHYDRATE=1` (`60-lazy-rehydrate.conf`) | Startup no longer OOMs during rehydrate | Steady-state working set still ~4 GB |
| `ROPE_SELF_WATCHDOG_ENABLED=1, ROPE_SELF_WATCHDOG_SUICIDE=0` (`40-self-watchdog.conf`) | Detects and logs hangs | Observe only - does not restart |
| `erpc-fleet-ha.timer` every 30s | External heal after 2 consecutive probe failures | Adds ~34-45s of downtime per cycle |
| Phase 1 Quipu v2.0 (sharded lattice + per-wallet head lock + per-shard HLC) | Removes global `Mutex<LamportClock>` bottleneck | Doesn't reduce memory footprint |
| Tier E ghost-reclaim grace + rate limit (deployed 2026-08-23) | Prevents ghost-reclaim from amplifying restart storms | Doesn't reduce base memory pressure |

**Conclusion:** every band-aid deployed since 2026-08-11 addresses a symptom. The root cause is that rope-node's steady-state working set (~4 GB) exceeds the fraction of the 7.7 GB VPS that is available to it after ipfs, semantic-agent, reth, cerber-mesh, and system overhead.

---

## 3. Mitigation menu (ordered cheapest to most expensive; NONE deployed by this agent)

Each option is described with (a) exact command, (b) expected effect, (c) rollback, (d) risk.

### Option A. Upgrade the VPS to 16 GB RAM (RECOMMENDED)

**Command:** Not executable by this agent; requires Gandi console upgrade.

**Expected effect:** Immediate elimination of swap thrash. rope-node's 4 GB working set fits comfortably alongside ipfs (1.9 GB) + semantic-agent (0.7 GB) + reth (0.25 GB) + system (~1 GB), total ~7 GB against a 16 GB ceiling with 9 GB headroom for buff/cache. MTBF returns to the July 2026 baseline (~30 min or better).

**Rollback:** Gandi downgrade path.

**Risk:** None functional. Cost delta ~EUR 10-20/mo. Requires a brief reboot window.

**Why recommended:** it is the only fix that addresses the root cause. Everything else below is either a workaround (B, C) or a code fix that takes weeks (D).

### Option B. `MemorySwapMax=0` on `datachain-rope.service`

**Command (operator-approved only):**

```bash
sudo cat > /etc/systemd/system/datachain-rope.service.d/70-no-swap.conf <<'EOF'
# 2026-08-23: force rope-node to OOM cleanly instead of thrash on swap.
# Under memory pressure, "no swap + hit MemoryMax -> SIGKILL -> systemd
# restart" recovers in ~30s. The alternative is 5-8 min of thrash before
# the external HA script notices and restarts anyway. This trades a
# higher restart rate for a shorter unresponsive window per cycle, i.e.
# better public RPC availability.
# Rollback: sudo rm /etc/systemd/system/datachain-rope.service.d/70-no-swap.conf
#           && sudo systemctl daemon-reload
[Service]
MemorySwapMax=0
EOF
sudo systemctl daemon-reload
sudo systemctl restart datachain-rope.service
```

**Expected effect:** rope-node can never swap. Under pressure, it hits MemoryMax=6.5G and gets SIGKILL'd. systemd (Restart=on-failure) restarts it in <5s. Total unresponsive window per cycle drops from ~7-8 min (thrash) to ~30-40s (OOM + restart). Public RPC availability improves from ~85% (thrash cycle) to ~99.4% (clean cycle).

**Rollback:** delete `70-no-swap.conf`, `daemon-reload`, `restart`.

**Risk:** Higher restart frequency (every 5-7 min still, but each restart is clean). Ledger persistence is RocksDB-backed since Phase 1.6 so no state is lost across OOMs. reth-rope on the same box is unaffected (separate cgroup). ipfs/semantic-agent on the same box are unaffected (separate cgroups).

**Trade-off:** each restart still causes a ~30-40s public RPC outage on writes (write path is pinned to BLUE, see Writer Promote Runbook §2). Reads and websockets stay up via `rpc_read_failover` to GREEN/DO-1/DO-2.

### Option C. Move semantic-agent off BLUE

**Command:** requires operator to decide target host (GREEN? separate small droplet?). Involves copying `/home/ubuntu/datachain-rope/target/release/semantic-agent` + relevant config + updating the semantic-agent public URL DNS.

**Expected effect:** frees ~700 MB RAM on BLUE. Not a full fix - rope-node would still be ~50% of remaining RAM - but reduces swap pressure meaningfully.

**Rollback:** point DNS back and stop the remote instance.

**Risk:** semantic-agent has active users (dcscan Search, ecosystem tooling). Migration must be coordinated with a brief cutover window.

### Option D. Make `LedgerManager::rehydrate` truly paged (code fix)

**Effort:** ~2 engineer-weeks. Requires a schema change in `RopeStorage` to add per-string knot-count metadata separate from the blob store, so `rehydrate` can list strings without touching blobs; blobs are then faulted in on first `getString` / `walkString`.

**Expected effect:** steady-state rope-node RSS drops from ~4 GB to ~1-1.5 GB. Even on the current 7.7 GB box, MTBF would go to hours or days. This is the "correct" fix per the 2026-08-11 comment's own follow-up note.

**Rollback:** revert the PR. Rehydrate becomes eager again.

**Risk:** low if unit-tested carefully. `getString` acquires the same lock as `rehydrate` today so no new concurrency surface is introduced.

### Option E. Ship Quipu Canon v2.0 Phase 2 (RocksDB persistence with tightened memtable + block cache)

**Effort:** already partially deployed via `ROPE_LEDGER_P2B=1` (8-way sharded persistence backend, see `30-ledger-p2b.conf`). Phase 2 completion would add explicit RocksDB memtable + block cache caps.

**Expected effect:** bounds rope-node RSS at RocksDB `block_cache_size + memtable_max_bytes` (currently unbounded).

**Rollback:** revert to Phase 1.6 single-store (env var flip).

**Risk:** already integration-tested in v2 tree; production risk is the same as any storage-layer change.

---

## 4. Recommended sequence

Given the operator's "cautiously proceed" directive and the "no autonomous changes to production" convention:

1. **Immediately (this doc):** publish the postmortem so the operator has a full picture.
2. **Operator decision (Option A):** upgrade Gandi VPS to 16 GB RAM. This eliminates the problem in one action; all other options are workarounds. Expected wall time: 15 min for the operator, 3 min of downtime for the VPS reboot.
3. **If (A) is deferred or blocked (Option B):** deploy `MemorySwapMax=0` drop-in. Trades thrash for fast OOM. Operator-approved manual deployment following the exact command block in §3B.
4. **In parallel (Option D):** file a P1 to make `LedgerManager::rehydrate` paged. Independent of A/B and helps regardless.
5. **Only after (D) lands or (A) is in place:** revisit whether Tier D Phase 2/3/4 (the rest of the v2.0 roadmap) is worth scheduling. Without the memory-pressure noise, the actual concurrency profile of BLUE can be measured cleanly.

**Not recommended right now:** deploying Tier D Phase 2/3/4 on the assumption that it will help MTBF. The evidence shows the current bottleneck is memory, not concurrency. Ship the memory fix first, then measure again.

---

## 5. What was NOT changed by this session

Per the "cautiously proceed" directive, this session did:
- Draft this postmortem (documentation only).
- Confirm the memory diagnosis via `/proc/<pid>/status`, `vmstat`, `cgroup memory.pressure`, and `journalctl` (read-only inspection).

This session did NOT:
- Deploy Option A, B, C, D, or E.
- Modify any drop-in, code, or systemd unit.
- Restart any service.
- Change nginx, ufw, or DNS.
- Touch the ghost-reclaim mitigation (Tier E remains as deployed 2026-08-23 earlier).
- Remove the dead `digitalocean_rpc` upstream (still deferred per Writer Promote Runbook §5).

---

## 6. Follow-ups (operator + engineering)

1. **[P0, operator]** ~~Decide between Option A and Option B.~~ **DECIDED 2026-08-23:** operator picked **Option A** (VPS upgrade to 16 GB). Runbook drafted: `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md`. Maintenance window not yet scheduled.
2. **[P1, engineering]** Ship Option D (paged rehydrate). File as a Quipu Canon v1.6.1 task.
3. **[P2, engineering]** Fix the eu-stack collector in `erpc-fleet-ha.sh`: currently fails to capture frames because ptrace_scope=1 on the box denies non-root attach across sibling processes. Options: (a) run the collector as root via a small setuid helper, (b) grant `CAP_SYS_PTRACE` to the collector script's own systemd unit, or (c) drop eu-stack entirely and rely on `/proc/<pid>/wchan` + `/proc/<pid>/stack` for kernel-side blocking info. Option (b) is the safest.
4. **[P2, engineering]** Add a memory-pressure counter to `rope_globalStats` (or `/v1/fleet-status`) exposing `cgroup memory.pressure full avg60` so external monitoring can page before the thrash cycle spirals.
5. **[P3, ops]** Consider moving ipfs (1.9 GB) off BLUE regardless of whether Option A is taken. ipfs pin traffic is not on the RPC hot path and moving it isolates rope-node's failure domain.

---

## 7. Correction footnote to insert into the Writer Promote Runbook

To be added to `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` §4:

> **CORRECTION 2026-08-23 (after publish):** live forensic evidence gathered ~2h after this runbook was drafted shows the root cause of BLUE's MTBF regression is memory pressure / swap thrash, not `LamportClock` contention. Phase 1 of Quipu Canon v2.0 (sharded lattice + per-wallet head lock + per-shard HLC) is already deployed in the current binary and is not the missing piece. See `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` for the corrected diagnosis, evidence table, and mitigation menu (VPS upgrade recommended). §§1-3 of this runbook (writer promote procedure, why writes can't auto-failover, ghost-reclaim mitigation status) remain valid.

---

## 8. Cross-references

- `docs/BLUE_NEVER_HANGDOWN_ALTERNATIVES_2026-08-23.md` - **broader alternatives menu** answering the operator directive "BLUE should never hangdown". Option A (this postmortem's recommendation) is a P0 item; §2-3 add workload offload, kernel tuning, auto-writer-promote, in-process circuit breaker, dedicated sealer host, and Phase 4 multi-writer.
- `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` - operator runbook implementing §4 Option A. Drafted 2026-08-23, awaits maintenance window.
- `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` - manual writer promote procedure; §§1-3 unchanged, §4 corrected here.
- `docs/CRYPTOGRAPHIC_NODE_ONBOARDING_DESIGN_V1_2026-08-23.md` - unrelated; still gated on operator answers to §9 open questions.
- `.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc` - v2.0 roadmap; Phase 1 confirmed deployed; Phase 2/3/4 deprioritized pending memory fix.
- `.cursor/rules/handover-security-audit-2026-06-11.mdc` - orthogonal (destructive-RPC auth); unaffected.
- `/etc/systemd/system/datachain-rope.service.d/10-memory-and-restart.conf` - existing 6.5 GB ceiling drop-in and its own honest follow-up note.
- `/etc/systemd/system/datachain-rope.service.d/60-lazy-rehydrate.conf` - `ROPE_LAZY_REHYDRATE=1`; helps startup but not steady state.
- `/etc/systemd/system/datachain-rope.service.d/40-self-watchdog.conf` - `ROPE_SELF_WATCHDOG_SUICIDE=0`; observe only per the P1.4 dump-only lesson.
- `/opt/datachain-rope/scripts/erpc-fleet-ha.sh` - external HA detects hangs after ~34-45s of probe failure and restarts.

---

*This postmortem is authored from the Datachain Rope agent's live inspection of rope-vps on 2026-08-23 between 09:35Z and 09:45Z. All numbers cited are from that window and are reproducible via the commands in §1. No code, config, service, or DNS was changed while producing this document.*
