# ROPE VPS memory upgrade runbook - Option A (Gandi 8 GB -> 16 GB)

**Author:** Datachain Rope agent
**Date:** 2026-08-23
**Purpose:** Execute the P0 fix from `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` §4 Option A.
**Blast radius:** BLUE-only. GREEN + DO-1 + DO-2 continue serving reads/WebSockets during the reboot window. All destructive writes are blocked during BLUE downtime (per `rpc_primary_only` pin - see `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` §2).
**Expected wall time:** 10-15 min (5-10 min operator work in Gandi control panel + 3 min reboot + 2 min verification).
**Cost impact:** ~+€40-60/month depending on the new Gandi flavor.

---

## 0. TL;DR

BLUE (`rope-vps`, `92.243.26.189`) is currently:
- Xen VM on Gandi Paris SD6
- 4 vCPU, 7.7 GiB RAM
- 16 GB swapfile at `/swapfile` currently 6.0 GiB in use
- 484 GB root disk (139 GB used, 345 GB free)

`rope-node` has a hard 6.5 GB memory ceiling (`10-memory-and-restart.conf` drop-in) but the resident set has grown to ~7 GB because `LedgerManager::rehydrate` loads the full v1 ledger index into memory at boot. Combined with `semantic-agent` (700 MB), `ipfs` (1.9 GB), `nginx`+`certbot`+`journald`+kernel (~1 GB), we exceed the 7.7 GiB RAM ceiling by ~800 MB, and the kernel resolves the pressure via swap - which then thrashes because the ledger index is hot.

Doubling RAM to 16 GiB removes the pressure entirely. Swap becomes unused, `pgmajfault`/s drops to near-zero, `LamportClock` contention (which was the 2026-05-03 suspicion in the v2.0 roadmap) is no longer a factor because there are no swap-induced multi-second stalls.

---

## 1. Pre-flight (operator, ~5 min before the maintenance window)

### 1.1 Confirm the target flavor in Gandi

Log in to https://admin.gandi.net (or the v5 IaaS console) and locate the `dcrope` VM. Confirm:

| Item | Expected value |
|---|---|
| Datacenter | Paris SD6 |
| Current flavor | Something like "V-R4" or similar (4 vCPU, 8 GB RAM) |
| Current disk | 484 GB (do NOT resize the disk in this window) |
| Snapshot policy | Note the last automatic snapshot timestamp |

Choose the target flavor: the next tier up on the RAM axis, **keeping vCPU at 4** (rope-node's write path is single-threaded per §Quipu-Canon-v2 pre-Phase-4; extra vCPU adds no value until Phase 4). Gandi flavors that fit:

- **Preferred: V-R8** or equivalent - 4 vCPU, 16 GB RAM. Doubles memory, keeps CPU/disk.
- Fallback: any flavor with 4+ vCPU and 16+ GB RAM if V-R8 is not currently available. Do NOT drop below 4 vCPU.

### 1.2 Announce the maintenance window

Post to the operator channel (or wherever ecosystem announcements go):

```
[MAINTENANCE] Datachain Rope BLUE (rope-vps) memory upgrade
Window: <UTC timestamp>  duration ~15 min
Impact:
  - Reads + WebSockets: UNAFFECTED (fail over to GREEN / DO-1 / DO-2 automatically)
  - Writes (eth_sendRawTransaction, destructive rope_*, txpool_*): BLOCKED for ~10 min
    Users will see the standard nginx pending-tx "will retry" behaviour;
    the write is retried against BLUE the moment it's back.
Post-upgrade: ~+8 GB RAM, expected end of the current MTBF regression.
```

### 1.3 Warn ecosystem peers (optional)

- DCSwap CERBER R12 will page on `edge.status=degraded` after the 180 s sustain threshold. Because reads stay healthy via failover, CERBER R12 should NOT fire. If it does, the failover is not working and this runbook §5 must be triggered.
- Tanastok mesh peer polls `/v1/fleet-status` every 2 min. It will log `writer.status=starting` during the reboot; that is expected.

### 1.4 Take a manual snapshot

In the Gandi console, take a manual pre-upgrade snapshot of the VM. Retain it for 7 days regardless of outcome.

---

## 2. Upgrade sequence (operator + agent, ~10 min)

### 2.1 Stop the service gracefully (agent-side)

Run from your laptop (SSH allowed to `rope-vps`):

```bash
ssh rope-vps 'set -euo pipefail
  echo "=== pre-stop memory snapshot ==="
  free -h
  cat /proc/$(pgrep -f "target/release/rope$" | head -1)/status 2>/dev/null | \
    grep -E "^(Vm|State):" || true
  echo
  echo "=== stopping datachain-rope ==="
  sudo systemctl stop datachain-rope.service
  sudo systemctl is-active datachain-rope.service || echo "confirmed stopped"
  echo
  echo "=== stopping dc-explorer to free RAM before reboot ==="
  sudo systemctl stop dc-explorer.service || true
  echo
  echo "=== stopping erpc-fleet-ha timer during reboot window ==="
  sudo systemctl stop erpc-fleet-ha.timer
  echo "=== ready for VM shutdown ==="'
```

Expected: `datachain-rope.service` in `inactive (dead)` state, `dc-explorer` stopped, `erpc-fleet-ha.timer` stopped so it does not attempt a restart during the flavor change.

### 2.2 Shut down the VM cleanly

```bash
ssh rope-vps 'sudo shutdown -h +1 "Datachain Rope BLUE memory upgrade - back in ~10 min"'
```

Wait 90 s. The SSH session will drop when the VM halts.

### 2.3 Resize in Gandi (operator, in the browser)

1. Confirm the VM is shown as **stopped** in the Gandi console.
2. Choose "Modify" (or the equivalent) -> new flavor (V-R8 or fallback).
3. Confirm the disk stays at 484 GB (Gandi may propose growing it - decline; disk is not the constraint).
4. Confirm the cost delta.
5. Click "Apply / Restart".
6. Wait for Gandi to report the VM as **running** on the new flavor. Typically 60-120 s.

### 2.4 Verify the resize landed and services are healthy

From your laptop:

```bash
# Wait for SSH to come back
until ssh -o ConnectTimeout=5 -o BatchMode=yes rope-vps 'true' 2>/dev/null; do
  echo "$(date -u +%H:%M:%S)  waiting for SSH..."
  sleep 5
done
echo "$(date -u +%H:%M:%S)  SSH ready"

ssh rope-vps 'set -euo pipefail
  echo "=== new memory ==="
  free -h
  echo
  echo "=== is /swapfile still there? ==="
  ls -la /swapfile
  cat /proc/swaps
  echo
  echo "=== bring services back up in the right order ==="
  sudo systemctl start datachain-rope.service
  sleep 5
  sudo systemctl start dc-explorer.service
  sleep 2
  sudo systemctl start erpc-fleet-ha.timer
  echo
  echo "=== service status ==="
  for svc in datachain-rope dc-explorer erpc-fleet-ha.timer; do
    printf "%-30s %s\n" "$svc" "$(systemctl is-active $svc)"
  done
  echo
  echo "=== post-boot memory (rope-node) ==="
  sleep 15
  ROPE_PID=$(pgrep -f "target/release/rope$" | head -1)
  if [ -n "$ROPE_PID" ]; then
    grep -E "^Vm" /proc/$ROPE_PID/status
  else
    echo "rope-node PID not yet available"
  fi'
```

Expected outputs:
- `free -h` shows `total: 15Gi` (or the exact new size)
- `/swapfile` still present and 0 or negligible use
- All 3 services `active`
- `rope-node VmRSS` around 6-7 GB (identical to before - this is expected and healthy; the fix is not that rope-node uses less RAM, it is that the OS no longer needs to swap)

### 2.5 Verify from outside (edge-side reader)

Wait 60 s after step 2.4, then from your laptop:

```bash
echo "=== fleet-status ==="
curl -sS https://erpc.datachain.network/v1/fleet-status | \
  jq '{writer: .writer.status, edge: .edge.status, sample_ok, sample_n, self_heal}'

echo
echo "=== chain tip ==="
curl -sS -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
  https://erpc.datachain.network | jq .

echo
echo "=== global stats (validates the ledger came back) ==="
curl -sS -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"rope_globalStats","params":[],"id":1}' \
  https://erpc.datachain.network | jq '.result | {total_strings, total_knots, invariant_holds}'
```

Expected:
- `writer.status: "healthy"` (may show "starting" for the first 30 s post-boot; wait a full minute if so)
- `edge.status: "healthy"`
- `sample_ok / sample_n = 10/10`
- `self_heal.escalate_to_cerber: false`
- `eth_blockNumber` returns a real hex block
- `rope_globalStats` returns `invariant_holds: true`

---

## 3. Post-upgrade verification (agent, first 2 hours)

This is the acceptance test that Option A actually removed the memory pressure. If any of these fail, roll back per §5.

### 3.1 Swap should stop being used

Within 30 min of the reboot, the swapfile in-use bytes should stop climbing and start dropping (kernel lazy-releases swap as pages get referenced back into RAM).

```bash
# T+30min, T+60min, T+120min: expect monotonically decreasing "used"
ssh rope-vps 'free -h; echo; cat /proc/swaps'
```

Acceptance: at T+2h, swap `used` is under 500 MB (target: 0).

### 3.2 pgmajfault rate should drop to near-zero

```bash
ssh rope-vps 'vmstat 5 3'   # look at the "si/so" (swap in / swap out) columns
```

Acceptance: `si` and `so` are 0 for all 3 samples.

### 3.3 memory.pressure should be flat

If cgroups memory pressure is exposed:

```bash
ssh rope-vps 'cat /sys/fs/cgroup/system.slice/datachain-rope.service/memory.pressure 2>/dev/null || \
              echo "cgroup pressure not exposed (kernel too old); skip"'
```

Acceptance: `full avg60` under 1.0 (was 15-40 during the thrash episodes per the postmortem §5).

### 3.4 MTBF should extend

Watch `NRestarts` over 6-24 hours:

```bash
ssh rope-vps 'systemctl show datachain-rope --property=NRestarts,ActiveEnterTimestamp'
```

Acceptance: no restart in 24 h. Compare to the 5-8 min MTBF measured pre-upgrade in the MTBF postmortem §2.

### 3.5 ghost-reclaim grace window rarely fires

Tier E's grace window (60 s post-restart) should almost never fire because the service is not restarting.

```bash
ssh rope-vps 'sudo tail -n 200 /var/log/erpc-fleet-ha.log | grep -c "grace_window" || echo 0'
```

Acceptance: fewer than 5 "grace_window" log lines in the 24 h after the upgrade (was firing on every timer tick during the thrash cycles).

---

## 4. Cross-project coordination (do these after §3 acceptance)

### 4.1 Cancel any pending writer-promote

If GREEN was pre-staged as a hot standby, no action is required - the promote runbook is manual and was not executed.

### 4.2 Update the MTBF postmortem

Add a `## 9. Outcome` section to `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` recording the actual observed MTBF over the first 72 h. The postmortem is intentionally left with an open ending so this outcome closes the loop.

### 4.3 Notify DCSwap / Tanastok

Drop a short `handover-to-*.mdc` in each workspace saying "BLUE memory upgraded, MTBF regression closed, no client action needed". This unblocks any peer that was throttling their traffic or padding deadlines because of BLUE flapping.

### 4.4 Consider un-blocking Phase A of node onboarding

The Cryptographic Node Onboarding Design v1 §9 open questions can be revisited now that BLUE is stable. Phase A is a good next task once the memory upgrade acceptance test in §3 passes.

---

## 5. Rollback (if the upgrade fails or regresses)

### 5.1 If the new flavor won't boot

- Gandi console -> restore from the pre-upgrade snapshot taken in §1.4.
- SSH back into rope-vps and repeat §2.4 verification.
- File an issue with Gandi support; keep the snapshot until diagnosed.

### 5.2 If the new flavor boots but rope-node fails to start

Check the drift on disk vs the pre-upgrade build:

```bash
ssh rope-vps 'set -euo pipefail
  systemctl status datachain-rope.service --no-pager
  echo
  sudo journalctl -u datachain-rope.service --since "-10 min" --no-pager | tail -60'
```

Most likely cause: a stale env file (`ROPE_LEDGER_PERSISTENCE`, `ROPE_LAZY_REHYDRATE`, etc.) - these are unchanged by the flavor resize, so unlikely. If genuinely broken, restore the snapshot per §5.1.

### 5.3 If MTBF does NOT improve after 24 h

Then Option A was insufficient and the actual regression has an additional cause. In that case:
- Layer on Option B (`MemorySwapMax=0` drop-in) per postmortem §4 Option B, which now acts as a safety net that turns any residual thrash into a clean OOM+restart cycle.
- Escalate to Option D (paged rehydrate) - engineering follow-up, not an ops action.
- Do NOT roll back the RAM upgrade - the extra RAM is only useful once regardless.

---

## 6. What this runbook does NOT do

- **Does not touch source code.** Zero code changes. The rope-node binary is unmodified.
- **Does not change Nginx / firewall / DNS.** All routing is unchanged.
- **Does not promote a new writer.** BLUE stays the sealer. If BLUE fails to come back on the new flavor, use `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` to promote GREEN.
- **Does not change the cost profile permanently.** The upgrade can be reversed at Gandi's next billing cycle if the operator wants to explore Options C/D instead.
- **Does not affect DCSwap, Tanastok, Datawallet+, or any peer.** They see BLUE go down for ~10 min via read-failover, no writes during that window, then BLUE comes back with more headroom.

---

## 7. Cross-references

- `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` - the postmortem this runbook resolves (§4 Option A).
- `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` - promote GREEN if the upgraded VM fails to boot (§5.1).
- `docs/CRYPTOGRAPHIC_NODE_ONBOARDING_DESIGN_V1_2026-08-23.md` - unlocked once BLUE is stable per §4.4.
- `.cursor/rules/datachain-rope-production-roadmap.mdc` - Gandi Paris SD6, port 41722 SSH.
- `/etc/systemd/system/datachain-rope.service.d/10-memory-and-restart.conf` - unchanged; the 6.5 GB ceiling is preserved. On a 16 GB host it will effectively never bind.
- `/opt/datachain-rope/scripts/erpc-fleet-ha.sh` - Tier E patched; grace window continues to protect ghost-reclaim after any future restart.

---

*This runbook is single-shot. Once §3 acceptance passes, archive it and update the MTBF postmortem §9 outcome.*
