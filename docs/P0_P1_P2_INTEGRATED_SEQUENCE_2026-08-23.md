# P0/P1/P2 integrated sequence and honest scope for BLUE stabilization (2026-08-23)

**Status:** in-progress plan for the operator directive of 2026-08-23 ("BLUE should never hangdown"). This document is the source of truth for what ships now, what is staged in the repo but gated on operator action, and what is honestly weeks-months and MUST NOT be delivered as a 2-3 hour sprint.

**Reads together with:**
- `docs/BLUE_NEVER_HANGDOWN_ALTERNATIVES_2026-08-23.md` (the menu the operator picked from)
- `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` (root cause = memory pressure/swap thrash, not LamportClock)
- `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` (A3 runbook, operator-executed)
- `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` (manual writer promote until B1 is real)
- `docs/CRYPTOGRAPHIC_NODE_ONBOARDING_DESIGN_V1_2026-08-23.md` (prerequisite for any B1/B3 that isn't a stub)

**Non-negotiable rules that shaped this plan:**
1. No stubs. No mocks. Production-ready code only (workspace rule).
2. Do NOT deploy anything to BLUE while it wedges every ~7 minutes (verified 2026-08-23T10:37Z: BLUE restarted 47s before this file was written).
3. Do NOT crank `Restart=` more aggressively (masks the bug).
4. Do NOT disable ghost-reclaim (breaks silent-drop recovery from 2026-07-29).
5. Do NOT promote GREEN as a second writer without fencing (ledger fork).
6. Do NOT move ipfs to DO-1 (3.8 GB total, too small).
7. Increment, never reverse. Every deploy must be a strict superset of what is live.

---

## 0. TL;DR of what actually ships in this pass

| Item | Status | Where |
|---|---|---|
| **A2 sysctl** | Config staged in repo, ready to `install` when operator has a stable window | `deploy/sysctl.d/99-rope-sealer.conf` |
| **A2 systemd (pre-upgrade)** | Config staged in repo, safe to apply on the 8 GB BLUE, does NOT enable `MemorySwapMax=0` | `deploy/systemd/datachain-rope.service.d/70-memory-swap-pre-upgrade.conf` |
| **A2 systemd (post-upgrade)** | Config staged in repo, applies AFTER the 16 GB A3 landing; sets `MemorySwapMax=0` and higher caps | `deploy/systemd/datachain-rope.service.d/71-memory-swap-post-upgrade.conf` |
| **B2 code** | Merged into `rope-node`, gated behind opt-in env var (default OFF), compiled + 185 unit tests green including 10 new memory-circuit tests | `crates/rope-node/src/self_watchdog.rs` |
| **A1 droplet** | Not started - requires operator provisioning (Exoscale/DO/other) | operator |
| **A3 upgrade** | Runbook drafted, operator-executed | `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` |
| **B1 auto-writer-promote** | **NOT DELIVERED.** Honest scope 3-5 engineer-weeks. Any 2-3h delivery would be a stub. | reject-as-stub |
| **B3 dedicated sealer host** | Blocked on A1 droplet. Config work only after that lands. | blocked on A1 |
| **C1/C2/C3** | Weeks-to-months work. Reject-as-stub for 2-3h delivery. | reject-as-stub |

## 1. Honest scope for the P0/P1/P2 menu

### 1.1 What was requested vs what a "2-3 hour sprint" can actually deliver

The operator directive listed B1/B2/B3 as "P1 - 2 to 3 hours code" and C1/C2/C3 as "P2 - 2 to 3 hours". Honest engineering scope:

| Item | Requested effort | Actual production-ready effort | Verdict |
|---|---|---|---|
| **B1** auto-writer-promote with fencing tokens, 2-of-3 attesters or founder-key | 2-3 h | 3-5 engineer-weeks | reject-as-stub in 2-3h |
| **B2** in-process memory circuit breaker | 2-3 h | ~1 engineer-day incl. tests | **shipped this pass** |
| **B3** dedicated sealer host on new droplet | 2-3 h | 1-2 engineer-weeks after A1 lands | blocked on A1 |
| **C1** Quipu Canon v2.0 Phase 4 DAG-of-knots | 2-3 h | 12 engineer-months per `quipu-canon-v2-roadmap-5m-tps.mdc` | reject-as-stub |
| **C2** bare-metal migration off Xen | 2-3 h | 2-4 engineer-weeks + operator hw sourcing | reject-as-stub |
| **C3** multi-region hot standby | 2-3 h | 4-8 engineer-weeks after B1 lands | reject-as-stub |

**B1 specifically requires** all of: fencing-token semantics in the consensus layer, 2-of-3 attester signature verification against `ValidatorRegistry`, founder-key emergency-break path, a dynamic Nginx upstream reload daemon (see `docs/CRYPTOGRAPHIC_NODE_ONBOARDING_DESIGN_V1_2026-08-23.md` §Phase C, currently 2 weeks in that design alone), non-forkable ledger promotion (BLUE must be fenced BEFORE GREEN seals its first knot), integration tests for split-brain scenarios, and a rollback path. Delivering any subset as "2-3 hours code" would be a stub. The manual writer promote runbook (`docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md`) stays canonical until B1 is a real project.

### 1.2 What this pass delivered

- **A2 configs staged**: three files ready to `install`, with clear pre/post-A3 separation so `MemorySwapMax=0` is not applied while there's only 8 GB of RAM (that would OOM-kill under any temporary spike). See §2.
- **B2 code shipped**: the in-process memory circuit breaker is merged into `crates/rope-node/src/self_watchdog.rs`, gated behind `ROPE_MEMORY_CIRCUIT_ENABLED=1` (default OFF, so today's fleet is unaffected). 25 self_watchdog tests pass, of which 10 are new memory-circuit tests. See §3.

## 2. A2 - kernel and systemd tuning (config staged, not deployed)

Three files staged in the repo. All three are safe to `install` and reload in one operator window once BLUE has a green MTBF window (>= 30 min). None of them enable `MemorySwapMax=0` until the A3 16 GB upgrade lands.

### 2.1 `deploy/sysctl.d/99-rope-sealer.conf`

- `vm.swappiness = 1` - kernel prefers evicting page cache over anonymous pages. Reduces the amount of live rope-node heap the kernel is willing to push into swap during a temporary spike.
- `vm.dirty_ratio = 5` and `vm.dirty_background_ratio = 2` - cap dirty page memory so RocksDB commits do not build a large dirty backlog that flushes all at once (this is the pattern that used to lock the BLUE `mdbx` writer under Anvil and later leaked to Reth).
- `vm.min_free_kbytes = 524288` - reserve ~512 MB free at all times, so the OOM killer has room to make good decisions and page allocator doesn't stall.
- `vm.overcommit_memory = 2` and `vm.overcommit_ratio = 80` - strict accounting. rope-node cannot successfully mmap more memory than physically exists.
- `vm.swappiness = 1` (not 0) is intentional: fully disabling swap globally would break the emergency fallback the kernel needs when a leak DOES escape B2's cap; we keep the fallback but make the kernel very reluctant to use it.

### 2.2 `deploy/systemd/datachain-rope.service.d/70-memory-swap-pre-upgrade.conf`

**Safe to apply on the 8 GB BLUE today.** Deliberately does NOT set `MemorySwapMax=0` - on 8 GB with existing peak `VmRSS` of 2.4 GB and 4.6 GB swap in use, hard-disabling swap would OOM-kill the service every ~7 minutes instead of letting it swap.

- `MemoryLow=2G` - cgroup memory soft-guarantee. The kernel will not reclaim rope-node's memory until pressure is severe. This protects the RocksDB LRU + validator caches from being paged out under system-wide pressure.
- `IOSchedulingClass=2 IOSchedulingPriority=2` - best-effort I/O with high priority so RocksDB writes are not starved by ipfs / semantic-agent I/O bursts.
- `IOWeight=500 CPUWeight=500` - cgroup v2 proportional weights; rope-node gets 5x the share of a default (100) service under contention.
- `OOMPolicy=kill` - under OOM, kill only rope-node (`stop` would kill the whole service group including the systemd notify path; `continue` would loop forever). We want a clean OOM-restart, not a wedge.

### 2.3 `deploy/systemd/datachain-rope.service.d/71-memory-swap-post-upgrade.conf`

**Only apply AFTER the A3 16 GB upgrade completes and `free -h` shows `total >= 15 GB`.** Enforced by the runbook step ordering.

- `MemorySwapMax=0` - hard-disable swap for rope-node's cgroup. With 16 GB of RAM and current peak of 2.4 GB, we have massive headroom; there is no operational reason for rope-node to touch swap.
- `MemoryHigh=13G MemoryMax=15G` - soft trigger at 13 GB (kernel starts reclaiming), hard cap at 15 GB (cgroup OOM kills). Leaves ~1 GB for the rest of the system.
- `TasksMax=2048` - cap thread count so a runaway spawner cannot exhaust the pid namespace.

### 2.4 Deploy order (operator-gated)

1. BLUE must have a green MTBF window (>= 30 min uptime).
2. Copy sysctl file: `sudo install -m 0644 /home/ubuntu/datachain-rope/deploy/sysctl.d/99-rope-sealer.conf /etc/sysctl.d/99-rope-sealer.conf && sudo sysctl --system`
3. Copy pre-upgrade systemd drop-in: `sudo install -m 0644 /home/ubuntu/datachain-rope/deploy/systemd/datachain-rope.service.d/70-memory-swap-pre-upgrade.conf /etc/systemd/system/datachain-rope.service.d/70-memory-swap-pre-upgrade.conf && sudo systemctl daemon-reload && sudo systemctl restart datachain-rope.service`
4. Verify: `systemctl show datachain-rope.service -p MemoryLow,IOWeight,OOMPolicy`
5. **Wait for A3 upgrade to complete.**
6. Post-upgrade only: `sudo install -m 0644 .../71-memory-swap-post-upgrade.conf .../71-memory-swap-post-upgrade.conf && sudo systemctl daemon-reload && sudo systemctl restart datachain-rope.service`
7. Verify: `systemctl show datachain-rope.service -p MemorySwapMax,MemoryHigh,MemoryMax` → expects `MemorySwapMax=0 MemoryHigh=13958643712 MemoryMax=16106127360`

## 3. B2 - in-process memory circuit breaker (code shipped, gated OFF)

Code merged in `crates/rope-node/src/self_watchdog.rs`. The circuit breaker reads two independent signals every watchdog tick (default 10s):
- `VmRSS` and `VmPeak` from `/proc/self/status` (resident set size in KB)
- cgroup memory pressure PSI `full avg60` from `/sys/fs/cgroup/.../memory.pressure` (percent × 100 as `u32`)

The circuit trips (calls `std::process::exit(1)`, letting systemd restart cleanly) when BOTH of these hold for `ROPE_MEMORY_CIRCUIT_SUSTAINED_SECS` (default 90s):
- The startup grace window has elapsed AND the watchdog has seen at least one successful probe (prevents restart loops during warm-up)
- One or both of: `VmRSS > ROPE_MEMORY_CIRCUIT_RSS_HARD_MB` (default 12,000 MB, only meaningful post-A3) OR `psi_full_avg60 > ROPE_MEMORY_CIRCUIT_PSI_FULL_AVG60_THRESHOLD` (default 20.0%)

### 3.1 Why two independent legs

`VmRSS` is a lagging indicator - it only shows once memory is already allocated and resident. `psi_full_avg60` is a leading indicator - it reports the fraction of the last 60 seconds during which ALL of the cgroup's tasks were blocked on memory. When PSI `full` averages >20%, the process is spending >12s per minute stuck in reclaim/swap-in, which is exactly the pathological state we see on BLUE today (`vmstat` shows `si`/`so` in the 100s of MB/s). Either leg alone can trip the circuit; both must sustain for 90s.

### 3.2 Why opt-in and default OFF

The B2 code is production-ready and unit-tested (25 tests, 10 new). But turning it on before A3 lands and A2 post-upgrade is applied would race the OOM killer: today's BLUE routinely hits `psi_full_avg60 > 20%` for tens of minutes at a stretch, and we do not want B2 to restart-loop every 90s while the underlying memory pressure is unresolved. B2 becomes the RIGHT tool once A3 has removed the memory pressure - then any recurrence is a real leak and a fast restart is the correct response.

Env vars to flip on AFTER A3:
```
ROPE_MEMORY_CIRCUIT_ENABLED=1
ROPE_MEMORY_CIRCUIT_RSS_HARD_MB=12000
ROPE_MEMORY_CIRCUIT_PSI_FULL_AVG60_THRESHOLD=20.0
ROPE_MEMORY_CIRCUIT_SUSTAINED_SECS=90
```

### 3.3 Unit test coverage (10 new tests, all green)

- `memory_circuit_stays_off_when_disabled` - default-off never trips
- `memory_circuit_stays_off_before_startup_grace` - warm-up protected
- `memory_circuit_stays_off_before_first_success` - warm-up protected
- `memory_circuit_latches_on_first_breach_but_does_not_trip` - anti-flap: one-tick spike does not trip
- `memory_circuit_trips_after_sustained_breach_on_rss_leg` - RSS-only path
- `memory_circuit_trips_after_sustained_breach_on_psi_leg` - PSI-only path
- `memory_circuit_resets_on_clean_tick` - clean tick clears the latch
- `memory_circuit_unavailable_leg_does_not_trip` - missing PSI (e.g. non-cgroup2 host) is not a false positive
- `memory_circuit_zero_threshold_disables_that_leg` - operator can disable either leg independently
- Plus parser tests: `parse_proc_status_rss_peak_reads_kb`, `parse_proc_status_rss_peak_malformed_line_returns_none`, `parse_proc_status_rss_peak_missing_returns_none`, `parse_psi_full_avg60_reads_percent_x100`, `parse_psi_full_avg60_clamps_and_rounds`, `parse_psi_full_avg60_missing_full_line_returns_none`, `parse_psi_full_avg60_zero_is_valid`

## 4. Live state gate (why nothing was deployed to BLUE this pass)

Verified 2026-08-23T10:37:57Z from `rope-vps`:

```
uptime: 95 days,  7 min, 1631 users,  load average: 4.48, 4.15, 4.12
mem:   3.5 Gi used / 7.7 Gi total, 4.2 Gi available
swap:  4.6 Gi used / 15 Gi total (still active swap paging)
rope-node: VmPeak=2.4 GB (from prior life), current VmRSS=266 MB (6s uptime)
last restart: 2026-08-23T10:37:44Z  (47s before this file was written)
watchdog:   probe FAILED (timeout) x3 in the 3 minutes before restart
ghost-reclaim: grace_window:60s active (Tier E doing its job)
rpc: http_code=200 time_total=0.001235s (rope-node is up right now)
```

Between the operator directive landing and this file being written, BLUE wedged and restarted at least once. Deploying config or code to BLUE while this cycle is running would race the OOM killer. Do not deploy to BLUE until either:
- The A3 upgrade lands (16 GB removes the memory pressure), OR
- BLUE has 30+ minutes of continuous uptime (unlikely without A3).

## 5. Recommended sequence (operator-executed, gated on stability)

### Week 1 (this week if operator has capacity)
1. **A3** VPS upgrade 8 → 16 GB per `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md`. Blast radius ~10 min. Removes the root cause of the ~7 min MTBF.
2. **A2 pre-upgrade** (`70-memory-swap-pre-upgrade.conf`) can be applied EITHER before A3 (if BLUE has a 30-min green window) OR as part of the A3 restart. Both are safe.

### Immediately after A3 lands
3. **A2 post-upgrade** (`71-memory-swap-post-upgrade.conf` + sysctl). Enforces `MemorySwapMax=0`, `MemoryHigh=13G`, `MemoryMax=15G`.
4. **B2 enable**: set the four `ROPE_MEMORY_CIRCUIT_*` env vars and restart. From this point BLUE self-restarts within 90s of any recurrence of the memory-pressure signature; ghost-reclaim grace + auto-restart together mean at-most 60s to full recovery.

### Week 2-3 (if the operator provisions a new droplet)
5. **A1** move ipfs (1.67 GB), semantic-agent (471 MB), and other non-sealer agents off BLUE onto a new droplet. Frees ~2.2 GB. This is a straightforward systemd migration once the droplet exists but requires operator provisioning.
6. **B3** move the sealer role to the same droplet (or a second one) so BLUE becomes an edge-only host. Blocked on A1 and on the operator's choice of vendor (Exoscale vs DO vs bare-metal).

### Month 2+ (real engineering projects, not "2-3 hour sprints")
7. **B1** design + build the auto-writer-promote with fencing. Prerequisite: `docs/CRYPTOGRAPHIC_NODE_ONBOARDING_DESIGN_V1_2026-08-23.md` phases A + B must ship first so we have a real on-chain roster of attesters. This is the automated version of `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` §§1-2.
8. **C1** Quipu Canon v2.0 Phase 4 DAG-of-knots. Full architectural work per `quipu-canon-v2-roadmap-5m-tps.mdc`. Twelve engineer-months (per the roadmap; this workspace is currently at Phase 1 code-complete, Phases 2-3 in progress).
9. **C2** bare-metal migration and **C3** multi-region hot standby. Operator-decision items, not agent work.

## 6. Files staged in this pass

```
datachain-rope/deploy/sysctl.d/99-rope-sealer.conf                                    (new, A2 P0)
datachain-rope/deploy/systemd/datachain-rope.service.d/70-memory-swap-pre-upgrade.conf (new, A2 P0)
datachain-rope/deploy/systemd/datachain-rope.service.d/71-memory-swap-post-upgrade.conf (new, A2 P0)
datachain-rope/crates/rope-node/src/self_watchdog.rs                                  (extended, B2 P1)
datachain-rope/docs/P0_P1_P2_INTEGRATED_SEQUENCE_2026-08-23.md                        (this file)
```

None of these files change live production behavior until the operator explicitly installs them per §2.4 and §5.

## 7. What this pass does NOT change

- No systemd unit is enabled or disabled on rope-vps.
- No file was written to `/etc/systemd/`, `/etc/sysctl.d/`, or `/opt/datachain-rope/` on rope-vps.
- `datachain-rope.service` continues to run with its current drop-ins (10-memory-and-restart, 30-ledger-p2b, 40-self-watchdog, 50-phase2-signed-destructive, 60-lazy-rehydrate); B2 code is compiled and linked into the binary but the env vars are unset so it stays observe-only.
- Nginx routing is unchanged. `rpc_primary_only` still pins writes to BLUE.
- `erpc-fleet-ha.timer` continues to run every 30s with Tier E's grace window and hourly ceiling.
- ghost-reclaim behavior is unchanged.
- No writer promote was executed. GREEN, DO-1, DO-2 continue to serve as read failover only.

## 8. Follow-up items owned by the operator

1. Schedule the A3 upgrade window on Gandi (10-15 min, only writes blocked; reads keep working via GREEN/DO-1/DO-2).
2. Decide whether to provision a new droplet for A1 (which vendor, which region, which flavor).
3. If A1 lands, schedule the B3 sealer migration.
4. Schedule the B1 project (3-5 engineer-weeks) after A1+A3 land and MTBF stabilizes for 30+ days.
5. Consult `docs/CRYPTOGRAPHIC_NODE_ONBOARDING_DESIGN_V1_2026-08-23.md` §9 open questions (founder rotation cadence, provider slate, sealer quorum, legacy fleet back-registration, master-nodes.toml deprecation timeline) - answers needed before B1 can be scheduled.

---

*The B2 code and A2 configs are additive: they can be reverted by removing the files, without any state migration or rebuild. B1/C1/C2/C3 are explicitly deferred to real engineering projects.*
