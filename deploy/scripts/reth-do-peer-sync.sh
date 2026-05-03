#!/usr/bin/env bash
# Reth DO-Peer Sync — runs ON one DO node, syncs state TO another DO node.
#
# Pattern: stop-tar-stream-restart (safe even with --dev divergent chains).
# Briefly stops the LOCAL Reth (~30-60s) to take a consistent MDBX snapshot,
# tar-streams over the DO-internal network (fra1↔fra1, ~80 MB/s), then both
# restart. Safe because the source node is itself a backup (no public traffic).
#
# Usage on source node (e.g. rpc-1 → rpc-2):
#   reth-do-peer-sync.sh root@167.172.106.174
#
# Cron on rpc-1 (sync to rpc-2 every 15 min):
#   12,27,42,57 * * * * /opt/datachain-rope/scripts/reth-do-peer-sync.sh root@167.172.106.174 >> /var/log/reth-do-peer-sync.log 2>&1

set -uo pipefail

DEST="${1:?Usage: $0 user@host}"
DATA_DIR="/opt/datachain-rope/reth"
LOCK_FILE="/tmp/reth-do-peer-sync-$(echo "$DEST" | tr '@.' '__').lock"
LOG_PREFIX="[reth-do-peer-sync $(date -u +%H:%M:%S) → $DEST]"

log() { echo "$LOG_PREFIX $1"; }

if [ -f "$LOCK_FILE" ]; then
    LOCK_AGE=$(( $(date +%s) - $(stat -c %Y "$LOCK_FILE" 2>/dev/null || echo "0") ))
    if [ "$LOCK_AGE" -gt 1800 ]; then
        log "WARN: Stale lock (${LOCK_AGE}s old), removing"
        rm -f "$LOCK_FILE"
    else
        log "Sync already running (lock age: ${LOCK_AGE}s), skipping"
        exit 0
    fi
fi
trap 'rm -f "$LOCK_FILE"' EXIT
touch "$LOCK_FILE"

log "=== DO-peer sync (stop-tar-stream-restart, source briefly down) ==="

# Verify destination reachable
DEST_PRE=$(ssh -n -o ConnectTimeout=5 -o BatchMode=yes "$DEST" \
    'curl -sf --max-time 3 -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin).get('result','?'))" 2>/dev/null)
log "Destination pre-sync block: $DEST_PRE"

# Verify source local Reth healthy before stopping
LOCAL_PRE=$(curl -sf --max-time 3 -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://127.0.0.1:8595 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)
[ -z "$LOCAL_PRE" ] && { log "ERROR: Local Reth not responding, aborting"; exit 1; }
log "Local source pre-sync block: $LOCAL_PRE"

log "Step 1: Stop dest Reth + clear data dir..."
ssh -n -o ConnectTimeout=8 -o BatchMode=yes "$DEST" \
    'systemctl stop reth-rope 2>/dev/null; rm -rf /opt/datachain-rope/reth/data/*; mkdir -p /opt/datachain-rope/reth/data; chown -R ubuntu:ubuntu /opt/datachain-rope/reth/data; echo READY' \
    || { log "ERROR: Could not prep dest"; exit 1; }

log "Step 2: Stop local Reth (source is backup, no public impact)..."
systemctl stop reth-rope
STOP_TS=$(date -u +%s)

log "Step 3: Stream tar local → dest (DO-internal fra1↔fra1)..."
STREAM_START=$(date +%s)
tar -C "$DATA_DIR" -cf - data \
    | ssh -o ConnectTimeout=10 -o BatchMode=yes "$DEST" \
        "tar -C $DATA_DIR -xf -"
STREAM_ELAPSED=$(( $(date +%s) - STREAM_START ))
log "  stream complete in ${STREAM_ELAPSED}s"

log "Step 4: Restart local Reth..."
systemctl start reth-rope
DOWN=$(($(date -u +%s) - STOP_TS))
log "  local was down ${DOWN}s"

log "Step 5: Fix ownership + start dest Reth..."
ssh -n -o ConnectTimeout=8 -o BatchMode=yes "$DEST" \
    'chown -R ubuntu:ubuntu /opt/datachain-rope/reth/data; systemctl reset-failed reth-rope.service 2>/dev/null; systemctl start reth-rope.service'

log "Step 6: Wait for dest to respond..."
for i in $(seq 1 30); do
    if ssh -n -o ConnectTimeout=4 -o BatchMode=yes "$DEST" \
        "curl -sf --max-time 3 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}' http://127.0.0.1:8595" >/dev/null 2>&1; then
        log "  dest responsive after $((i*2))s"
        break
    fi
    sleep 2
done

DEST_POST=$(ssh -n -o ConnectTimeout=5 -o BatchMode=yes "$DEST" \
    'curl -sf --max-time 5 -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin).get('result','?'))" 2>/dev/null)
log "Destination post-sync block: $DEST_POST"

log "=== DO-peer sync done (local down ${DOWN}s) ==="
