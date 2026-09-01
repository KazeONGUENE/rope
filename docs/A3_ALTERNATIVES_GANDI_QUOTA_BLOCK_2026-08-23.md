# A3 alternatives -  Gandi 20/20 core quota block (2026-08-23)

**Context:** operator attempted the A3 upgrade per `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` and hit:

> An error occurred while updating the configuration of dcrope. Raw error: Quota exceeded for cores: Requested 4, but already used 20 of 20 cores

A3 (BLUE 8 GB -> 16 GB) is a prerequisite for two downstream deploys that are already coded and staged:

- P0-A2 `71-memory-swap-post-upgrade.conf` (`MemorySwapMax=0`, `MemoryHigh=13G`, `MemoryMax=15G`)
- P1-B2 `72-memory-circuit-breaker.conf` (`ROPE_MEMORY_CIRCUIT_ENABLED=1`, 12 GB RSS hard-line, 50% PSI, 90 s sustained)

Both are held in `deploy/systemd/datachain-rope.service.d/` and MUST NOT be applied on today's 8 GB BLUE (they would restart-loop the swap-thrashing node).

## What raising quota buys, in one line

**Enables the last two staged mitigations** to land, giving BLUE:
- No swap at cgroup level (kernel kills the service instead of thrashing).
- Explicit RSS hard-line (12 GB) with 90 s sustained-breach circuit break, before the kernel OOM killer chooses a victim thread mid-block-anchor.

Without A3, BLUE stays on P0-A2 pre-upgrade + P0-A1 (IPFS offload) -  which is meaningful improvement (already deployed, verified), but not the full stabilization the operator ordered.

## Option A -  Gandi support ticket (quota raise)

| Item | Value |
|---|---|
| Effort | 5 min to file, 1-7 days to land |
| Cost | +€40-60/mo (2x2 core delta = one Gandi flavor tier bump) |
| Risk | zero |
| Downtime | zero (in-place resize per runbook, ~10-15 min once quota clears) |
| Blast radius | writes on BLUE for the resize window (per runbook §2) |

Ticket text template:

> Hello, our production node `dcrope` (VM 5xxxxxx.gandi.cloud, 4c/8G/500G, region SD6-Paris) needs a resize to 4c/16G to fix a memory-pressure incident (swap thrash on legitimate 2.6 GB working set). We currently show 20/20 cores used across 4 VMs and the resize request is blocked. Please raise our core quota from 20 to 24 for the account. This is a single-VM RAM bump -  the core count on `dcrope` itself does not change.

Fastest path if operator wants zero migration risk.

## Option B -  Decommission a Gandi VM to free 4 cores

BLUE and the other 3 Gandi VMs total 20 cores (the ceiling). We do not know their names/roles without operator confirmation, but the ecosystem inventory says the other Gandi VMs are:

- `anvil-vps` / `dcrope-node2` (GREEN, 8 GB, 4 core) -  SEALER STANDBY, keep
- `tanastok-vps` (92.243.24.244) -  SEALER of `tanastok.io`, keep
- `dcswap-vps` (92.243.26.114) -  SEALER of dcswap.net, keep

**None of the other 3 are safely decommissionable.** GREEN is the writer-promote target. Tanastok and DCSwap are separate-project production. If the operator identifies any legacy/idle Gandi VM not in this list, decommissioning frees 2-4 cores instantly and the runbook resize proceeds.

## Option C -  Migrate BLUE to DigitalOcean (DO API key provided)

### Sizing data (verified 2026-08-23T11:32Z via DO API)

BLUE current footprint:

| Component | Disk | RAM (working set) |
|---|---|---|
| Reth data (`/opt/datachain-rope/reth/data`) | ~11 GB (5.7 static + 4.9 db + 393 M rocksdb) | 2-3 GB |
| Source + build (`/home/ubuntu/datachain-rope`) | 24 GB | n/a |
| IPFS (moving to rope-offload-01 already) | 7.8 GB | 1.67 GB (freed by P0-A1) |
| Everything else (`/opt/datachain-rope/*` minus reth + ipfs) | ~5 GB | ~500 MB |
| **Total** | **~48 GB after A1** | **~4-5 GB after A1** |

Disk-wise BLUE fits comfortably on any 200+ GB droplet.

### Candidate droplet sizes (DO fra1, verified available, cheapest first)

| Slug | RAM | vCPU | Disk | Price | Fit |
|---|---|---|---|---|---|
| `s-4vcpu-16gb-amd` | 16 GB | 4 (AMD) | 200 GB | $84/mo | 4-core parity with Gandi, 200 GB fits BLUE with 4x headroom |
| `s-4vcpu-16gb-320gb-intel` | 16 GB | 4 (Intel) | 320 GB | $96/mo | +Intel + bigger disk |
| `s-8vcpu-16gb` | 16 GB | 8 | 320 GB | $96/mo | double the CPU headroom (may help under future load) |
| `s-8vcpu-32gb-amd` | **32 GB** | 8 | 400 GB | $168/mo | future-proof (Phase 4 DAG-of-knots may need it) |

### DO account status

- Account `contact@datachain.one`, active, `droplet_limit: 25`, currently at **12 droplets** -  13 slots free.
- fra1 VPC `datachain-rope-vpc` (10.10.10.0/24) already contains: validators 1-3, rpc-1/2, dcswap-failover, rope-relay-01, rope-offload-01.
- Second VPC `10.20.0.0/20` for the rope-cluster-* Phase 3 fleet.

### Migration effort (ballpark)

| Phase | What | Wall time | Operator involvement |
|---|---|---|---|
| Bootstrap | Provision `rope-vps-do` droplet, install Reth + rope-node, systemd units, nginx, firewall | 2-3 h | mostly automated by rope agent |
| Data seed | `rsync` reth data (~11 GB after `du`), source tree, systemd drop-ins | ~30 min | maintenance window |
| Cutover | Stop BLUE, final delta rsync (writes blocked), start DO, DNS repoint | 15-30 min | maintenance window |
| Verify | Reth head advances, 3 timers healthy (`erpc-fleet-ha`, watchdog, ipfs), MTBF over 24 h | ongoing | monitoring |

**Total blast radius: ~30 min writes-blocked**, comparable to the Gandi resize but with a real hostname change (rope-vps -> `rope-vps-do` in `~/.ssh/config`, and DNS if the operator wants).

### Risks specific to migration

1. **Reth `known-peers.json` + rocksdb are cwd-sensitive** -  rsync must preserve mode + `--links`, and the new box must have the same paths (`/opt/datachain-rope/reth/data`) to avoid re-syncing from peers.
2. **Nginx config on BLUE currently pins writes to `host.docker.internal:8545`** which is Docker-network-specific. Migration must confirm the same Docker networking exists on DO or rewrite to loopback.
3. **fleet-status DNS records** (`erpc.datachain.network`, `ws.datachain.network`) point at 92.243.26.189 (Gandi). Post-migration the DNS repoint is what moves live traffic.
4. **DO Intel vs Gandi Xen** -  different hypervisor may expose different swap/PSI behavior. Runbook §3 acceptance criteria (`memory.pressure full avg60 < 1.0`, `pgmajfault ~= 0`) still apply.
5. **UFW rules** -  BLUE has a mature rule set; must be migrated 1:1 or attesters lose connectivity.

### Non-risks (things the operator does NOT need to worry about)

- Foundation Ed25519 keys are on-box files under `/opt/datachain-rope/keys/` (mode 0600) -  rsync moves them with the tree.
- Reth `--dev` mode = deterministic, so a re-sync from peers is safe if the rsync misses a block; block hash + parent chain stays canonical.
- All ecosystem callers (Tanastok, DCSwap, wallets) reach BLUE through `erpc.datachain.network` -  a DNS repoint is transparent to them.

## Recommendation

**Land P0-A1 (IPFS offload cutover) FIRST**, regardless of which A3 path is picked. The cutover runbook is Phase B in the P0/P1/P2 plan, needs ~15 min maintenance window, and frees 1.67 GB RAM + 7.8 GB disk on BLUE immediately. It's ready as of 2026-08-23T~13:00Z (rope-offload-01 fully bootstrapped, metadata rsync done).

**Then, in operator's preferred order:**

1. **Option A (Gandi quota raise)** if the ticket-and-wait is acceptable -  this is the "one-line change to the runbook" path.
2. **Option C (DO migration)** if the operator wants to reduce Gandi lock-in permanently, and is willing to schedule a ~30 min maintenance window. `s-4vcpu-16gb-amd` at $84/mo is the like-for-like replacement.
3. **Option B** only if the operator identifies a specific legacy Gandi VM I don't know about.

**Do NOT** execute options A/B/C autonomously. Each has permanent side effects (billing, DNS, key material) that need operator sign-off.

## Cross-references

- `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` -  the runbook that hit the quota
- `docs/P0_P1_P2_INTEGRATED_SEQUENCE_2026-08-23.md` -  the plan A3 sits in
- `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` -  why A3 matters (root cause = memory pressure)
- `docs/BLUE_NEVER_HANGDOWN_ALTERNATIVES_2026-08-23.md` -  the full menu of BLUE-stabilization options
- `.cursor/rules/handover-p0-p1-p2-sequence-2026-08-23.mdc` -  concise summary for future agent sessions
