#!/usr/bin/env bash
# reth-snapshot-replicate.sh - Datachain Rope BLUE->followers replication via consistent on-line snapshot.
#
# Replaces the broken reth-blue-green-sync.sh (which hot-rsync'd live mdbx ->
# torn snapshot -> mdbx corruption on every restart of the follower).
#
# Mechanism:
#   1. `reth db copy --compact -p <DEST>` - bundled mdbx_copy. Produces a
#      consistent point-in-time snapshot file with zero BLUE downtime.
#      MVCC throttle (-p) keeps memory pressure bounded.
#   2. For each follower, in parallel: stop services, rsync snapshot, restart.
#   3. If a follower's chain hash at the test block matches BLUE's, skip it
#      (follower is in sync; resync is unnecessary).
#
# Cron (every 10 min, offset to avoid mass-restarts on the hour):
#   7,17,27,37,47,57 * * * * /opt/datachain-rope/scripts/reth-snapshot-replicate.sh >> /home/ubuntu/log/reth-snapshot-replicate.log 2>&1
#
# 2026-05-20: created after BLUE outage postmortem (handover-blue-outage-2026-05-20-postmortem.mdc).

set -uo pipefail

LOCK_FILE=/tmp/reth-snapshot-replicate.lock
DATA_DIR=/opt/datachain-rope/reth/data
SNAPSHOT_PATH=/opt/datachain-rope/reth/snapshot
RETH_BIN=/usr/local/bin/reth
CHAIN_SPEC=/opt/datachain-rope/reth/genesis.json
LOG_DIR=/home/ubuntu/log
mkdir -p "$LOG_DIR"

log() { echo "[reth-snap $(date -u +%H:%M:%S)] $*"; }

# Followers - must be reachable from the active sealer (new-blue) via SSH.
# Paris legacy Gandi host uses non-standard SSH port 41722.
declare -A FOLLOWERS=(
  ["GREEN"]="ubuntu@92.243.25.119"
  ["DOrpc1"]="root@157.230.18.45"
  ["DOrpc2"]="root@167.172.106.174"
  ["ParisLegacy"]="ubuntu@92.243.26.189"
)
declare -A FOLLOWER_SSH_PORT=(
  ["ParisLegacy"]="41722"
)

follower_ssh() {
  local name="$1"
  shift
  local port="${FOLLOWER_SSH_PORT[$name]:-22}"
  ssh -p "$port" -n -o ConnectTimeout=15 -o StrictHostKeyChecking=no "$@"
}

# === Lock ===
if [ -f "$LOCK_FILE" ]; then
  AGE=$(( $(date +%s) - $(stat -c %Y "$LOCK_FILE" 2>/dev/null || echo 0) ))
  if [ "$AGE" -gt 1800 ]; then
    log "WARN: stale lock (${AGE}s), removing"
    rm -f "$LOCK_FILE"
  else
    log "skip - running for ${AGE}s already"
    exit 0
  fi
fi
trap 'rm -f "$LOCK_FILE"' EXIT
touch "$LOCK_FILE"

OVERALL_START=$(date +%s)
log "=== START ==="

# Get BLUE's reference block at a recent height
PRIMARY_BLOCK_HEX=$(curl -sf -m 3 -X POST -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://127.0.0.1:8595 | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"])' 2>/dev/null)
if [ -z "$PRIMARY_BLOCK_HEX" ]; then
  log "ERROR: BLUE reth not responding; abort"
  exit 1
fi
PRIMARY_BLOCK=$(( PRIMARY_BLOCK_HEX ))
TEST_BLOCK=$(( PRIMARY_BLOCK - 100 ))
TEST_HEX=$(printf '0x%x' $TEST_BLOCK)
BLUE_HASH=$(curl -sf -m 3 -X POST -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getBlockByNumber\",\"params\":[\"$TEST_HEX\",false],\"id\":1}" \
    http://127.0.0.1:8595 | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"]["hash"])' 2>/dev/null)
log "BLUE@$PRIMARY_BLOCK; reference block $TEST_BLOCK hash $BLUE_HASH"

# === Decide which followers need a resync ===
NEEDS_SYNC=()
for name in "${!FOLLOWERS[@]}"; do
  target="${FOLLOWERS[$name]}"
  ip=$(echo "$target" | cut -d@ -f2)
  TIP_HEX=$(timeout 6 curl -sf -m 4 -X POST -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://$ip:8545 2>/dev/null | python3 -c 'import sys,json; print(json.load(sys.stdin)["result"])' 2>/dev/null || true)
  if [ -z "$TIP_HEX" ]; then
    log "[$name] WARN: unreachable at $ip:8545 - SKIP (do not wipe a node we cannot probe)"
    continue
  fi
  TIP=$(( TIP_HEX ))
  if [ "$TIP" -lt "$TEST_BLOCK" ]; then
    log "[$name] tip $TIP < reference $TEST_BLOCK (lag $((TEST_BLOCK - TIP)) blocks) - will resync"
    NEEDS_SYNC+=("$name")
    continue
  fi
  H=$(timeout 6 curl -sS -m 4 -X POST -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getBlockByNumber\",\"params\":[\"$TEST_HEX\",false],\"id\":1}" \
    http://$ip:8545 2>/dev/null | python3 -c 'import sys,json; r=json.load(sys.stdin).get("result"); print(r["hash"] if r else "")' 2>/dev/null || true)
  if [ -z "$H" ]; then
    log "[$name] WARN: no hash at $TEST_HEX despite tip $TIP - will resync"
    NEEDS_SYNC+=("$name")
  elif [ "$H" = "$BLUE_HASH" ]; then
    log "[$name] chain hash matches BLUE @ $TEST_BLOCK - skipping"
  else
    log "[$name] hash $H != BLUE - will resync"
    NEEDS_SYNC+=("$name")
  fi
done

if [ ${#NEEDS_SYNC[@]} -eq 0 ]; then
  log "All followers in sync; no work to do."
  exit 0
fi

# === Phase 1: snapshot ===
log "Phase 1: reth db copy --compact -p (zero downtime; ~6 min)"
rm -rf "$SNAPSHOT_PATH"
SNAP_START=$(date +%s)
$RETH_BIN db --datadir "$DATA_DIR" --chain "$CHAIN_SPEC" \
  copy --compact -p "$SNAPSHOT_PATH" >/dev/null 2>&1
log "snapshot done in $(($(date +%s) - SNAP_START))s"

# === Phase 2: parallel push to followers that need it ===
PIDS=()
for name in "${NEEDS_SYNC[@]}"; do
  target="${FOLLOWERS[$name]}"
  (
    F_START=$(date +%s)
    echo "[$name] starting at $(date -u +%H:%M:%S)"
    follower_ssh "$name" "$target" "
      sudo systemctl stop rope-evm-attester datachain-rope reth-rope 2>/dev/null
      sleep 3
      sudo rm -rf $DATA_DIR/db
      sudo mkdir -p $DATA_DIR/db
      sudo chown ubuntu:ubuntu $DATA_DIR/db
      printf '2' | sudo tee $DATA_DIR/db/database.version > /dev/null
      sudo chown ubuntu:ubuntu $DATA_DIR/db/database.version
    "
    rsync_rsh=""
    if [[ "${FOLLOWER_SSH_PORT[$name]:-22}" != "22" ]]; then
      rsync_rsh="ssh -p ${FOLLOWER_SSH_PORT[$name]} -o StrictHostKeyChecking=no"
    fi
    if [[ -n "$rsync_rsh" ]]; then
      rsync -a -e "$rsync_rsh" "$SNAPSHOT_PATH" "$target:$DATA_DIR/db/mdbx.dat"
    else
      rsync -a "$SNAPSHOT_PATH" "$target:$DATA_DIR/db/mdbx.dat"
    fi
    follower_ssh "$name" "$target" "sudo chown ubuntu:ubuntu $DATA_DIR/db/mdbx.dat"
    if [[ -n "$rsync_rsh" ]]; then
      rsync -a -e "$rsync_rsh" "$DATA_DIR/static_files/" "$target:$DATA_DIR/static_files/"
    else
      rsync -a "$DATA_DIR/static_files/" "$target:$DATA_DIR/static_files/"
    fi
    follower_ssh "$name" "$target" "sudo chown -R ubuntu:ubuntu $DATA_DIR/static_files"
    # jwt.hex / reth.toml stay node-local (Engine-API).
    follower_ssh "$name" "$target" "
            sudo systemctl start reth-rope
      sleep 10
      sudo systemctl start rope-evm-attester 2>/dev/null || true
      sudo systemctl start datachain-rope
    "
    echo "[$name] done in $(($(date +%s) - F_START))s"
  ) > "$LOG_DIR/reth-snap-$name.log" 2>&1 &
  PIDS+=($!)
done

for pid in "${PIDS[@]}"; do
  wait "$pid" || log "WARN: a follower bootstrap exited non-zero (pid $pid)"
done

# Concatenate per-follower logs into main output
for name in "${NEEDS_SYNC[@]}"; do
  cat "$LOG_DIR/reth-snap-$name.log" | sed "s/^/    /"
done

rm -rf "$SNAPSHOT_PATH"

log "=== END (total $(($(date +%s) - OVERALL_START))s) ==="
