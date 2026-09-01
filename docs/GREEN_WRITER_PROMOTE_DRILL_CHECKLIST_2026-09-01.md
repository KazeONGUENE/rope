# GREEN Writer-Promote Drill Checklist (2026-09-01)

**Status:** DRAFT - tabletop + staged drill only until operator schedules a maintenance window.
**Supersedes IP/host references in:** `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` section 2 (still canonical for rationale; use **this** doc for current fleet topology).
**Purpose:** Repeatable operator checklist when London (BLUE) is wedged > 5 min and GREEN must become the sealer.

---

## 0. Fleet topology (post 2026-08-24 London migration)

| Slot | Host | IP | SSH | Role today |
|---|---|---|---|---|
| **BLUE (writer + edge)** | `new-blue` / DO lon1 | `159.65.208.206` | `ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206` | Sealer, nginx (`rope-nginx`), fleet-status publisher, dc-explorer |
| **GREEN (promote target)** | `anvil-vps` / Gandi Paris | `92.243.25.119` | `ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119` | Attester follower; **first** manual promote candidate |
| **DO-rpc-1** | `datachain-rpc-1` | `157.230.18.45` | `ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45` | Attester follower |
| **DO-rpc-2** | `datachain-rpc-2` | `167.172.106.174` | `ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174` | Attester follower |
| **Paris legacy** | `rope-vps` / Gandi Paris | `92.243.26.189` | `ssh -p 41722 -i ~/.ssh/DCRope_key ubuntu@92.243.26.189` | Attester follower (snapshot-synced; **not** a promote target unless GREEN unavailable) |

Public surfaces:

| URL | Writer pin? | Failover |
|---|---|---|
| `https://erpc.datachain.network` | Writes -> BLUE loopback only | Reads -> GREEN, DO-1, DO-2, Paris |
| `https://ws.datachain.network` | N/A (subscriptions) | Same 4 attesters as reads |
| `https://erpc.datachain.network/v1/read` | Writes rejected (405) | Attesters only (no BLUE) |
| `https://erpc.datachain.network/v1/fleet-status` | N/A | Published from London HA timer |

DNS: `erpc.datachain.network` A -> `159.65.208.206` (TTL 300). Failover watcher default BLUE IP is London (`deploy/scripts/erpc-dns-failover-watcher.sh`).

---

## 1. When to run this checklist (production)

Run **only** if **all** are true:

- [ ] London `datachain-rope` has been wedged **> 5 minutes** (Restart loop, or TCP accepts but `eth_blockNumber` times out > 3s).
- [ ] `curl -sS https://erpc.datachain.network/v1/fleet-status | jq '.writer.status'` is **not** `healthy` for the full 5 min window.
- [ ] Tier E ghost-reclaim is **not** in a grace backoff (`journalctl -t erpc-fleet-ha | tail -20` - no active `SKIP grace=` for a false wedge).
- [ ] Operator has **sealer key material** ready to install on GREEN (see section 3.3).
- [ ] Maintenance comms sent (DCSwap, Tanastok, internal) - section 8.

**Do NOT promote** if:

- [ ] London is recovering on its own (block height advancing on loopback).
- [ ] Only reads are slow - reads already fail over automatically; no promote needed.
- [ ] Sealer key is missing on GREEN and cannot be transported out-of-band within 30 min.

---

## 2. Drill modes

| Mode | What you exercise | Production impact |
|---|---|---|
| **A - Tabletop** | Walk sections 3-7 on paper; verify SSH, key paths, nginx file locations | None |
| **B - Read-only preflight** | Run section 3 commands only; record block heights | None |
| **C - Staged promote (maintenance window)** | Full sections 4-6 in a booked window with partners on standby | Writes blocked during fence; ~10-15 min |

Recommended cadence: **Mode A quarterly**, **Mode B after any nginx or HA script change**, **Mode C once before first real promote**.

---

## 3. Preflight (~5 min)

Run from operator workstation. Record outputs in the drill log (section 9).

### 3.1 Confirm London is wedged (not just slow)

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'systemctl status datachain-rope --no-pager | head -20'

ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'curl -s --max-time 3 http://127.0.0.1:8545 -X POST -H "content-type: application/json" \
   -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}"'
```

- [ ] `datachain-rope` not `active (running)` **or** JSON-RPC times out / empty result.

### 3.2 Capture reference block heights

```bash
BLUE=$(ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'curl -s --max-time 3 http://127.0.0.1:8545 -X POST -H "content-type: application/json" \
   -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}"' \
  | jq -r .result)

GREEN=$(curl -s --max-time 3 http://92.243.25.119:8545 -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r .result)

echo "BLUE=$BLUE GREEN=$GREEN"
python3 - <<'PY'
import os
b=int(os.environ.get("BLUE","0x0"),16) if os.environ.get("BLUE") not in (None,"null","") else -1
g=int(os.environ.get("GREEN","0x0"),16)
print("gap_blocks", b-g if b>=0 else "BLUE unreadable")
PY
```

- [ ] GREEN lag **<= 512 blocks** (attester HA resync threshold). If lag > 512, run `reth-snapshot-replicate.sh` from London first or **ABORT** promote.

### 3.3 Sealer / proposer readiness on GREEN

**Production model (2026-09-01):** block production is **`rope-evm-proposer.service`** (rope-engine-driver), not `ROPE_ENABLE_MINING` + `sealer.key`. London runs the sole active proposer; GREEN runs attester only until promote.

```bash
# Legacy sealer.key check (may be absent on engine-driver fleets)
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119 \
  'test -f /opt/datachain-rope/data/sealer.key && echo SEALER_KEY_PRESENT || echo SEALER_KEY_ABSENT'

# Engine-driver promote prerequisites (canonical)
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119 \
  'test -x /opt/datachain-rope/bin/rope-engine-driver && echo ENGINE_DRIVER_OK || echo ENGINE_DRIVER_MISSING; \
   echo -n "proposer unit: "; systemctl is-enabled rope-evm-proposer 2>/dev/null || echo not-installed; \
   echo -n "attester unit: "; systemctl is-active rope-evm-attester 2>/dev/null || echo inactive'

ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'systemctl is-active rope-evm-proposer rope-evm-attester'
```

- [ ] `ENGINE_DRIVER_OK` on GREEN.
- [ ] London `rope-evm-proposer` is `active` (only one proposer fleet-wide before promote).
- [ ] GREEN `rope-evm-proposer` is `disabled` or `inactive` pre-promote (expected today).
- [ ] If using legacy `sealer.key` path instead, result must be `READY` - otherwise **STOP** and transport key OOB.

### 3.4 Fleet-status + edge corroboration

```bash
curl -sS https://erpc.datachain.network/v1/fleet-status \
  | jq '{writer:.writer, edge:.edge, self_heal:.self_heal}'
```

- [ ] `writer.status` matches observed London state.
- [ ] `edge.status` noted (reads may still be `healthy` via attesters).

### 3.5 Backup nginx config on London (before any edit)

```bash
TS=$(date -u +%Y%m%dT%H%MZ)
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  "cp /opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf \
      /opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf.pre-promote-$TS"
```

- [ ] Backup path recorded: `________________________`

---

## 4. Fence London (~30 s)

**Goal:** Ensure London cannot resume sealing while GREEN starts.

```bash
# Disable auto-restart path before stop (critical)
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'systemctl mask datachain-rope.service && systemctl stop datachain-rope.service'

sleep 5
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'timeout 3 bash -c "echo > /dev/tcp/127.0.0.1/8545" && echo STILL_UP || echo FENCED'
```

- [ ] Output is `FENCED`.
- [ ] `erpc-fleet-ha.timer` on London may still run - confirm it does **not** unmask/start `datachain-rope` during promote (mask prevents start).

Optional (if wedge was watchdog-driven):

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'systemctl stop rope-node-watchdog.service 2>/dev/null; systemctl mask rope-node-watchdog.service 2>/dev/null || true'
```

---

## 5. Promote GREEN (~2 min)

### 5.1 Enable sealer on GREEN

**Engine-driver fleet (canonical since London migration):** move the proposer, do not enable legacy mining env vars unless the host still uses them.

```bash
# Stop proposer on London first (after fence in section 4)
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'systemctl stop rope-evm-proposer.service && systemctl mask rope-evm-proposer.service'

# Enable proposer on GREEN
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119 \
  'sudo systemctl unmask rope-evm-proposer.service 2>/dev/null; \
   sudo systemctl enable --now rope-evm-proposer.service && \
   systemctl is-active rope-evm-proposer rope-evm-attester datachain-rope reth-rope'
```

**Legacy fallback** (only if `rope-evm-proposer` is not installed on GREEN):

```bash
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119 'sudo tee /etc/systemd/system/datachain-rope.service.d/50-sealer.conf' <<'EOF'
[Service]
Environment="ROPE_ENABLE_MINING=1"
Environment="ROPE_SEALER_KEYSTORE=/opt/datachain-rope/data/sealer.key"
EOF

ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119 \
  'sudo systemctl daemon-reload && sudo systemctl restart datachain-rope.service'
```

- [ ] `rope-evm-proposer` (or legacy `datachain-rope` sealer mode) is `active` on GREEN.
- [ ] London proposer is **stopped and masked**.

### 5.2 Verify GREEN seals

```bash
sleep 15
H1=$(curl -s --max-time 3 http://92.243.25.119:8545 -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r .result)
sleep 5
H2=$(curl -s --max-time 3 http://92.243.25.119:8545 -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq -r .result)
echo "H1=$H1 H2=$H2"
```

- [ ] `H2 > H1` (height advancing). If not, **goto section 7 Rollback**.

---

## 6. Repoint London nginx (~30 s)

Edit on **London** (edge stays on London; only write upstream moves):

File: `/opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf`

Change `upstream rpc_primary_only` from loopback to GREEN:

```nginx
upstream rpc_primary_only {
    server 92.243.25.119:8545 max_fails=3 fail_timeout=5s;  # GREEN promoted YYYY-MM-DD
    keepalive 64;
    keepalive_requests 10000;
    keepalive_timeout 300s;
}
```

Optional (recommended): make GREEN first in `rpc_read_failover` and `rope_ws` primary slots so edge reads prefer the new writer.

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'docker exec rope-nginx nginx -t && docker exec rope-nginx nginx -s reload'
```

Verify public writes:

```bash
curl -sS --max-time 5 https://erpc.datachain.network -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' | jq .
```

- [ ] HTTP 200, `.result` advances on repeat calls.
- [ ] Test `eth_sendRawTransaction` with a known-safe read-only rejection is **not** required; optional signed no-op only in maintenance window.

Update fleet-status writer id (manual until HA script learns GREEN-as-writer):

- [ ] Edit `/opt/datachain-rope/scripts/erpc-fleet-ha.sh` writer metadata if hardcoded to `blue` / London loopback.
- [ ] Run `erpc-fleet-ha.sh` once or wait for timer tick; confirm `/v1/fleet-status` reflects GREEN.

---

## 7. Rollback (if promote fails)

```bash
TS=<backup timestamp from 3.5>
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  "cp /opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf.pre-promote-$TS \
      /opt/datachain-rope/code/deploy/nginx/conf.d/datachain.network.conf && \
   docker exec rope-nginx nginx -s reload"

ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119 \
  'sudo rm -f /etc/systemd/system/datachain-rope.service.d/50-sealer.conf && \
   sudo systemctl daemon-reload && sudo systemctl restart datachain-rope.service'

ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'systemctl unmask datachain-rope.service && systemctl start datachain-rope.service'
```

- [ ] London loopback RPC responds.
- [ ] Public `erpc.datachain.network` writes succeed again.

---

## 8. Post-promote announce (~5 min)

### 8.1 On-chain audit knot (loopback on GREEN)

```bash
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.25.119 \
  'curl -s -X POST http://127.0.0.1:8545 -H "content-type: application/json" \
   -d "{\"jsonrpc\":\"2.0\",\"method\":\"rope_appendToLedger\",\"params\":[\"0x000000000000000000000000000000000000d002\",{\"interaction_type\":\"WriterPromoteEvent\",\"description\":\"Sealer promoted from BLUE (London 159.65.208.206) to GREEN (anvil-vps 92.243.25.119)\",\"metadata\":{\"from_host\":\"new-blue\",\"from_ip\":\"159.65.208.206\",\"to_host\":\"anvil-vps\",\"to_ip\":\"92.243.25.119\",\"operator\":\"<name>\",\"drill_or_incident\":\"incident\"}}],\"id\":1}"'
```

- [ ] Knot hash recorded: `________________________`

### 8.2 Retire writer credentials on fenced London

**Engine-driver fleet:** stop and mask `rope-evm-proposer.service` on London (section 5.1). Do **not** delete `jwt.hex` on followers.

**Legacy sealer.key fleets only:**

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@159.65.208.206 \
  'shred -u /opt/datachain-rope/data/sealer.key 2>/dev/null || rm -f /opt/datachain-rope/data/sealer.key'
```

- [ ] Only after GREEN sealing verified for >= 3 blocks.

### 8.3 Partner handover (template)

Drop `handover-writer-promote-green-live-YYYY-MM-DD.mdc` into DCSwap + Tanastok `.cursor/rules/`:

- Writer moved: London -> GREEN
- Public RPC URL unchanged (`https://erpc.datachain.network`)
- Writes now land on GREEN; reads unchanged
- No client RPC URL changes required
- Monitor `ghost_reclaim` and migration relayer for 24h

---

## 9. Drill log (copy per exercise)

| Field | Value |
|---|---|
| Date (UTC) | **2026-09-01T09:16Z** |
| Mode (A/B/C) | **B** (read-only preflight) |
| Operator | Cursor agent / operator workstation |
| Trigger (real / drill) | **drill** |
| BLUE block at start | **0x4123c3 (4,268,995)** |
| GREEN block at start | **0x4123c3 (0 lag)** |
| Paris block at start | **0x412338 (139 lag - OK)** |
| DO-1 / DO-2 | **0x4123c3 / 0x4123c4** |
| Lag blocks (GREEN) | **0** (threshold 512) |
| Sealer key READY? | **N/A - engine-driver model** (`sealer.key` absent both hosts) |
| Engine-driver GREEN | **proposer disabled, attester active, binary OK** |
| London proposer | **active** |
| Fleet-status | **writer=healthy, edge=healthy 10/10, escalate=false** |
| Fence confirmed? | skipped (Mode B) |
| GREEN sealed? | skipped |
| Nginx reloaded? | skipped |
| Public write OK? | **eth_chainId 0x425d4 OK** |
| WriterPromoteEvent knot | skipped |
| Rollback needed? | no |
| Notes | Paris RPC briefly down mid-run (reth+rope stopped 09:10Z, recovered by 09:15Z); attester HA log showed `rpc_probe_fail` while local RPC was down. Section 3.5 nginx backup skipped (read-only). GREEN :8545 not reachable from public internet (firewall - use SSH loopback). |

---

## 10. Acceptance criteria (drill complete)

- [ ] All preflight commands run without SSH surprises.
- [ ] Operators know where **proposer / validator credentials** live (engine-driver model):
  - London: `/opt/datachain-rope/reth/jwt.hex` (Engine-API), `rope-evm-proposer.service` env in `/etc/systemd/system/rope-evm-proposer.service.d/`
  - GREEN promote: enable `rope-evm-proposer.service` on GREEN; **no** `sealer.key` required when engine-driver is active
  - Legacy fallback only if `sealer.key` exists: `/opt/datachain-rope/data/sealer.key` (shred on fenced writer after promote)
- [ ] Nginx backup/restore path verified.
- [ ] Partner comms template reviewed.
- [ ] Rollback section timed (< 5 min) in Mode C.

---

## 11. Cross-references

- Full rationale + Tier D context: `docs/WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md`
- London migration: `docs/BLUE_MIGRATION_TO_DO_LON1_RUNBOOK_2026-08-23.md`
- Read failover (already live): `deploy/nginx/conf.d/datachain.network.conf` `rpc_read_failover`, `rope_ws`
- Attester HA (Paris): `deploy/systemd/erpc-fleet-ha.env.d/paris-attester.conf`
- Read-pool drain (P0): `deploy/scripts/read-pool-drain-follower.sh`, `deploy/nginx/conf.d/includes/read-pool/`
- Resilience runbook: `docs/FLEET_RESILIENCE_P0_P1_2026-09-01.md`
- Automated promote (future B1): `docs/BLUE_NEVER_HANGDOWN_ALTERNATIVES_2026-08-23.md`

---

*Last updated 2026-09-01 after Paris attester HA + London read-pool expansion. Revisit after any writer host or sealer-key rotation.*
