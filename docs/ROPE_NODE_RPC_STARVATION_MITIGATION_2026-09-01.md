# Rope-node RPC starvation mitigation (2026-09-01)

## Problem

On **new-blue** (32 GB RAM, 0 swap), `datachain-rope.service` was restarting every
~6-7 minutes because `erpc-fleet-ha.timer` saw loopback `eth_blockNumber` time out.
Each restart drops WSS handshakes on `ws.datachain.network` / `ws.datachain.network`
while nginx reconnects to BLUE `:8546`.

This is **not** the 2026-08-23 8 GB swap-thrash MTBF. Evidence:

| Signal | Observation |
|---|---|
| VmRSS at hang | 6.4-7.0 GB (cgroup MemoryMax 24 GB) |
| PSI memory.full avg60 | 0 |
| Restarter | `erpc-fleet-ha.sh` (`rpc_probe_fail`), not OOM |
| Attesters during failure | `peer_ok=1` (GREEN/DO healthy) |
| Correlation | Hang ~1-2 min after 5-min entity-manifest refresh tick |

**Root cause class:** synchronous CPU + lock work on the Tokio RPC worker pool
(entity manifest rebuild of 1,626 labels) combined with restart-on-probe-fail policy
that ignores healthy attesters.

## What we rejected (external hypothesis)

| Prescription | Why not on this deployment |
|---|---|
| `TimeoutStopSec=5s` | Risks RocksDB / ledger corruption (`ROPE_LEDGER_P2B=1`, intentional 30s stop) |
| `proxy_read_timeout 2s` on WSS | Breaks long `eth_getLogs` and subscription streams |
| `fail_timeout=2s` on upstream | Excessive re-probing of a recovering BLUE |
| Separate jsonrpsee runtime | Hand-rolled `rpc_server.rs`; not a drop-in |
| Restart every 90s while peers OK | **Worse for WSS** than degraded drain |

Nginx WSS failover shipped earlier (upstream `max_fails`, `proxy_next_upstream`) stays
in place and is complementary.

## Sustainable fix (three layers)

### Layer 1 - Stop starving Tokio (code)

1. **`entity_manifest::apply_response` on `spawn_blocking`** - 1,626-entity registry
   rebuild no longer runs on RPC workers during the 5-min Tanastok refresh tick.
2. **Background tip refresh** - every 2s, a dedicated task updates `block_number`
   from Reth with a 1s budget (bypasses the HTTP handler queue).
3. **`eth_blockNumber` / `rope_knotIndex` fast path** - 800ms delegation budget,
   then serve the background-refreshed cache. HA probes succeed even under load.

Files: `crates/rope-node/src/entity_manifest.rs`, `crates/rope-node/src/rpc_server.rs`.

### Layer 2 - Peer-aware HA policy (ops)

When failure is **`rpc_probe_fail` only** (not `block_stall`) and **`peer_ok=1`**:

- Publish writer status **`degraded`** instead of **`unhealthy`**
- Require **`ROPE_HA_PEER_DEFER_FAIL_THRESHOLD=20`** (~10 min) before restart
- Force restart after **`ROPE_HA_PEER_DEFER_MAX_SECS=900`** (15 min)

Nginx already routes reads/WSS to attesters; this stops the restart storm.

Files: `deploy/scripts/erpc-fleet-ha.sh`,
`deploy/systemd/erpc-fleet-ha.env.d/peer-defer-restart.conf`.

### Layer 3 - Forensics (ops)

`capture_dump()` now tries **`gdb -batch thread apply all bt`** when `eu-stack`
returns empty (YAMA ptrace_scope=1).

### Layer 4 - Decoupled loopback probe (code + ops)

**Post-deploy finding (2026-09-01 ~01:37Z):** Layers 1-2 stopped the restart storm
(`peer_defer`, NRestarts=0) but loopback `eth_blockNumber` still timed out at 3015ms
when VmRSS hit ~7 GB under DCSwap `rope_appendToLedger` load. The fast-path cache
does not help if new TCP connections never reach a free Tokio worker.

**Fix:** `probe_listener.rs` binds **`127.0.0.1:8544`** on a dedicated **OS thread**
with blocking `TcpListener`. It reads `handlers.block_number` via `parking_lot` only:

- `GET /healthz` -> `{"ok":true}`
- `GET /v1/tip` -> `{"ok":true,"block_hex":"0x..."}`

`erpc-fleet-ha.sh` uses **`ROPE_BLUE_PROBE_URL=http://127.0.0.1:8544/v1/tip`**
instead of POST `:8545 eth_blockNumber` for writer liveness. **Block stall** detection
unchanged (frozen tip vs advancing peer still triggers heal).

Env: `ROPE_PROBE_LISTEN` (optional override), `ROPE_BLUE_PROBE_URL` (HA).

## Deploy checklist (new-blue writer)

```bash
# 1. Backup binary + HA script
cp /home/ubuntu/datachain-rope/target/release/rope ~/backup-$(date -u +%Y%m%dT%H%M%SZ)/rope-pre-rpc-starvation
cp /opt/datachain-rope/scripts/erpc-fleet-ha.sh ~/backup-$(date -u +%Y%m%dT%H%M%SZ)/

# 2. Sync + rebuild rope-cli on host (or rsync prebuilt jammy binary)
cd /home/ubuntu/datachain-rope && cargo build --release -p rope-cli

# 3. Restart rope-node during a planned window
sudo systemctl restart datachain-rope.service

# 4. HA script + env
sudo cp deploy/scripts/erpc-fleet-ha.sh /opt/datachain-rope/scripts/
sudo cp deploy/systemd/erpc-fleet-ha.env.d/peer-defer-restart.conf /etc/erpc-fleet-ha.env.d/
sudo systemctl restart erpc-fleet-ha.timer

# 5. Acceptance (30+ min)
journalctl -u datachain-rope.service --since -30m | grep -c 'Started datachain-rope'  # expect 0-1
grep peer_defer /var/log/erpc-fleet-ha.log | tail
curl -sS http://127.0.0.1:8544/v1/tip   # expect {"ok":true,"block_hex":"0x..."} in <5ms
curl -sS http://127.0.0.1:8544/healthz
```

## Follow-ups (not in this pass)

- Audit sync `rope_*` read handlers that walk ledgers on async workers (`spawn_blocking`)
- Deploy `ws_subscription_bridge` for public `eth_subscribe` (separate track)
- Enable memory circuit on 32 GB only if RSS runaway reappears (PSI > 0); current
  wedge shows PSI=0 at 7 GB RSS - circuit would not have fired
