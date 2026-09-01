# Quipu Canon v2.0 Phase 2.B - Parallel RocksDB Writer Deploy Playbook

**Status:** PREP-COMPLETE, GATED ON OPERATOR APPROVAL.
**Date drafted:** 2026-08-12
**Cross-refs:**
- Design + full section-22 doc: `.cursor/rules/handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc`
- Roadmap: `.cursor/rules/quipu-canon-v2-roadmap-5m-tps.mdc`
- Source: `crates/rope-storage/src/rocksdb_persistence_p2b.rs`
- Systemd drop-in: `deploy/systemd/datachain-rope.service.d/30-ledger-p2b.conf`

---

## TL;DR

Phase 2.B replaces the single-flusher RocksDB persistence path with an
8-way sharded writer pool, gated behind `ROPE_LEDGER_P2B=1`. Default
remains off. On-disk format is unchanged so rollback is a single-line
env-remove + restart with no data migration.

The change is fully additive: the legacy `RocksPersistence` module is
untouched; the new `RocksPersistenceP2b` module lives beside it; the
`LedgerStore` layer dispatches at open time via an internal enum.

**Impact target:** eliminate the residual ~6-9 min wedge/restart
cadence observed on BLUE by removing the single global RocksDB
flusher as the append-path bottleneck.

**Measurement:** compare pre- and post-P2B `rope_latticeMetrics`
histograms (`head_guard_hold` mean + p99 + max).

---

## Pre-flight

### 1. Baseline captured

Pre-P2B snapshots of `rope_latticeMetrics` are being collected on
production BLUE at:

```
deploy/p2b-baseline/pre-p2b-lattice-metrics-<timestamp>.json
deploy/p2b-baseline/sample-<timestamp>.json   (30x, one per minute)
deploy/p2b-baseline/baseline-capture.log
```

Aggregate them with `deploy/p2b-baseline/summarise.py` (below) before
deploy so we have a hard number for the pre-P2B `head_guard_hold`
distribution.

### 2. Local tests green

```bash
cd datachain-rope
cargo test -p rope-storage --lib          # expect 61/61
cargo test -p rope-node --lib             # expect 169/169
cargo check --workspace                    # only pre-existing warnings
```

### 3. Fleet health at deploy start

```bash
curl -sS https://erpc.datachain.network/v1/fleet-status | jq '
  {
    writer: .writer.status,
    edge: .edge.status,
    escalate: .self_heal.escalate_to_cerber,
    ha_restarts_last_hour: .ha.restarts_last_hour
  }
'
```

Expected before deploy:
- `writer` != `out_of_service`
- `edge.status` == `healthy` or `degraded` (not `down`)
- `escalate` == `false`

If `writer=out_of_service`, do NOT deploy. Reset the HA state file
first (see section 20.4 of handover doc).

### 4. Confirm no other rope-node changes in flight

```bash
ssh rope-vps 'cd /home/ubuntu/datachain-rope && git status'
```

Only P1 files (`self_watchdog.rs`, `lattice_metrics.rs`,
`ledger_manager.rs`, `rpc_server.rs`, `rpc_auth.rs`,
`ledger_manager.rs` Phase C changes) should show as modified.

---

## Deploy sequence

### Step 1 - Sync source

```bash
cd /Users/kazealphonseonguene/Downloads/DATACHAIN\ ROPE/datachain-rope
rsync -av \
  crates/rope-storage/src/rocksdb_persistence_p2b.rs \
  crates/rope-storage/src/lib.rs \
  crates/rope-storage/src/rocksdb_persistence.rs \
  rope-vps:/home/ubuntu/datachain-rope/crates/rope-storage/src/
```

### Step 2 - Backup + build on jammy

```bash
# Backup current production binary
ssh rope-vps 'cp /home/ubuntu/datachain-rope/target/release/rope \
  ~/backup-2026-08-12/rope-pre-phase-2b-p2b-$(date -u +%Y%m%dT%H%M%SZ)'

# Build on the jammy VPS (per build-on-jammy policy)
ssh rope-vps 'export PATH="$HOME/.cargo/bin:$PATH" && \
  cd /home/ubuntu/datachain-rope && \
  cargo build --release -p rope-cli 2>&1 | tail'
```

Expected: `Finished release` line, only pre-existing warnings.

### Step 3 - Install systemd drop-in

```bash
scp deploy/systemd/datachain-rope.service.d/30-ledger-p2b.conf \
  rope-vps:/tmp/30-ledger-p2b.conf
ssh rope-vps 'sudo mkdir -p /etc/systemd/system/datachain-rope.service.d && \
  sudo cp /tmp/30-ledger-p2b.conf \
    /etc/systemd/system/datachain-rope.service.d/30-ledger-p2b.conf && \
  sudo systemctl daemon-reload'
```

### Step 4 - Restart (activates both new binary and P2B flag)

```bash
ssh rope-vps 'sudo systemctl restart datachain-rope.service && \
  sleep 8 && \
  systemctl is-active datachain-rope.service'
```

Expected output: `active`

### Step 5 - Smoke checks

```bash
# a) Loopback RPC responds
ssh rope-vps 'curl -sS -X POST http://127.0.0.1:8545 \
  -H "content-type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}"'

# b) No panics / errors in the first 100 log lines
ssh rope-vps 'sudo journalctl -u datachain-rope.service -n 100 --no-pager \
  | grep -iE "p2b|rocksdb|panic|error" | head -30'

# c) lattice_metrics counter starts advancing (proves append path runs
#    under the new backend)
sleep 30 && curl -sS -X POST https://erpc.datachain.network \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"rope_latticeMetrics","params":[],"id":1}' \
  | jq '.result.per_op'
```

If any smoke check fails, roll back per section below.

---

## Soak phase (>=24h)

Start a post-P2B sample capture the moment the restart completes:

```bash
# Reuse the same capture script, output to post-p2b-* directory
cat > /tmp/rope-postp2b-capture.sh << 'EOF'
#!/bin/bash
OUT_DIR="/Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/datachain-rope/deploy/p2b-baseline/post-p2b"
mkdir -p "$OUT_DIR"
LOG="$OUT_DIR/postp2b-capture.log"
echo "== capture start $(date -u +%FT%TZ) ==" > "$LOG"
# 24h = 1440 samples at 60s each
for i in $(seq 1 1440); do
  ts=$(date -u +%Y%m%dT%H%M%SZ)
  f="$OUT_DIR/sample-${ts}.json"
  http=$(curl -sS -o "$f" -w '%{http_code} %{time_total}s' \
    -X POST https://erpc.datachain.network \
    -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","method":"rope_latticeMetrics","params":[],"id":1}' 2>&1)
  echo "[$(date -u +%FT%TZ)] sample $i/1440 http=$http" >> "$LOG"
  sleep 60
done
EOF
chmod +x /tmp/rope-postp2b-capture.sh
nohup /tmp/rope-postp2b-capture.sh > /tmp/rope-postp2b-capture.stdout 2>&1 &
```

### Success criteria (evaluated at t+24h)

| Metric | Baseline (pre-P2B) | Success threshold |
|---|---|---|
| `head_guard_hold.mean_ns` (append_to_ledger op) | ~1.5 ms | drop by >=30% |
| `head_guard_hold.max_ns` (steady-state, ignoring startup) | ~5-8 ms | max <2 ms sustained |
| `erpc-fleet-ha.log` HEAL_ISSUED count/24h | ~10/hr on BLUE | <=2/hr |
| CERBER `self_heal.escalate_to_cerber` flag | flipping every 6-9 min | stays false the full 24h |
| RSS ceiling on BLUE | ~4.3 GB peak | stays <=4.5 GB |
| RPC p99 latency (external probe) | occasional 5-8 s spikes | steady <100 ms |

### Ongoing observation commands

```bash
# 1. Watch HEAL_ISSUED cadence
ssh rope-vps 'sudo tail -f /var/log/erpc-fleet-ha.log | grep -E "HEAL_ISSUED|restart"'

# 2. Watch RSS on BLUE
watch -n 30 "ssh rope-vps 'ps -o pid,rss,%mem,cmd -C rope | head'"

# 3. Check fleet-status for escalation flag
watch -n 60 "curl -sS https://erpc.datachain.network/v1/fleet-status | \
  jq '.writer.status, .self_heal.escalate_to_cerber, .self_heal.unhealthy_for_secs'"
```

---

## Staged rollout (after 24h green on BLUE)

Repeat steps 1-5 on:
1. GREEN
2. DO-rpc-1
3. DO-rpc-2

with a shorter (~4h) soak between each. All four nodes must run the
same backend during any given period; mixed operation is safe
(on-disk state is identical) but complicates measurement.

Once all four are on P2B, update `deploy/scripts/deploy-fleet.sh` to
include the systemd drop-in in the fleet package.

---

## Rollback

### Immediate (env-flag revert)

Reverts to legacy `RocksPersistence` backend. On-disk data is
byte-compatible; the legacy backend safely ignores the per-shard
watermark keys the P2B backend added.

```bash
ssh rope-vps 'sudo rm /etc/systemd/system/datachain-rope.service.d/30-ledger-p2b.conf && \
  sudo systemctl daemon-reload && \
  sudo systemctl restart datachain-rope.service'
```

### Deeper (binary revert)

Only if a bug in the new binary itself surfaces (not just the P2B
code path).

```bash
ssh rope-vps 'sudo systemctl stop datachain-rope.service && \
  cp ~/backup-2026-08-12/rope-pre-phase-2b-p2b-<timestamp> \
     /home/ubuntu/datachain-rope/target/release/rope && \
  sudo systemctl start datachain-rope.service'
```

---

## Known caveats (documented at design time)

1. **Cross-shard atomicity is not guaranteed.** Two writes from the
   same `append_to_ledger` call may land on different shards. This is
   mitigated by (a) `await_durable` on both writes before the RPC
   caller sees a response, and (b) lazy rehydration handling of a
   descriptor pointing at a not-yet-persisted blob. Detailed proof
   sketch in handover section 22.4.
2. **RocksDB WAL fsync throughput is the physical floor.** All 8
   shard flushers write to the same WAL. If the underlying disk is
   I/O-bound, further scaling requires multiple RocksDB instances
   (different data dirs). Not currently a bottleneck on rope-vps.
3. **Wedge is not guaranteed to go away 100%.** Phase 2.B removes the
   single global flusher lock, which is the currently-measured
   dominant bottleneck. Any remaining source of stall (e.g. filesystem
   fsync latency, external RPC dependency, or an as-yet-unmeasured
   in-process lock) is not addressed. The soak's job is to quantify
   the residual.

---

## Files touched

| Path | Change | Deployed? |
|---|---|---|
| `crates/rope-storage/src/rocksdb_persistence_p2b.rs` | NEW module | pending |
| `crates/rope-storage/src/lib.rs` | Enum dispatch + `p2b_backend_enabled()` | pending |
| `crates/rope-storage/src/rocksdb_persistence.rs` | `queue_cap()` + `chain_key()` made `pub(crate)` | pending |
| `deploy/systemd/datachain-rope.service.d/30-ledger-p2b.conf` | NEW systemd drop-in | pending |
| `deploy/p2b-baseline/` | Pre-P2B measurement corpus | in-progress |
| `docs/QUIPU_CANON_V2_PHASE2B_DEPLOY_PLAYBOOK.md` | This document | LOCAL |

---

## Deploy log (fill in on deploy day)

```
=== BLUE deploy ===
Deployer:
Start:
Baseline aggregated: [ ] yes / hash:
Files synced:
Build result:
Drop-in installed:
Restart timestamp:
Smoke checks: [ ] loopback RPC [ ] no panic in logs [ ] per_op counter advancing
Soak start:
Soak end:
Result: [ ] promoted to GREEN [ ] rolled back [ ] extended soak

=== GREEN deploy ===
(after BLUE 24h green)
...

=== DO-rpc-1 deploy ===
...

=== DO-rpc-2 deploy ===
...
```
