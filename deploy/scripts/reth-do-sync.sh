#!/usr/bin/env bash
# Reth Blue-DO State Sync — Primary → DigitalOcean rpc-1
#
# Hot rsync (no BLUE downtime) — same pattern as reth-blue-green-sync.sh.
# DO Reth is briefly stopped during the rsync window to avoid mdbx
# concurrent-write corruption on the destination side.
#
# IMPORTANT: This script assumes DO has been bootstrapped with a clean
# state via /opt/datachain-rope/scripts/reth-do-bootstrap.sh. If DO mdbx
# is corrupt or empty, run the bootstrap script first.
#
# Run: every 15 minutes via cron (offset 7 min from blue-green sync)
# Cron: 7,22,37,52 * * * * /opt/datachain-rope/scripts/reth-do-sync.sh >> /var/log/datachain-rope-reth-do-sync.log 2>&1

set -uo pipefail

LOCK_FILE="/tmp/reth-do-sync.lock"
DATA_DIR="/opt/datachain-rope/reth/data"
DO_HOST="root@157.230.18.45"
DO_DATA="/opt/datachain-rope/reth/data"
LOG_PREFIX="[reth-do-sync $(date -u +%H:%M:%S)]"

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

log "=== Starting blue-DO sync (hot rsync, no BLUE downtime) ==="

PRIMARY_BLOCK_HEX=$(curl -sf -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://127.0.0.1:8595 | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)

if [ -z "$PRIMARY_BLOCK_HEX" ]; then
    log "ERROR: Primary RPC not responding, aborting"
    exit 1
fi

PRIMARY_BLOCK=$((${PRIMARY_BLOCK_HEX}))
log "Primary at block $PRIMARY_BLOCK ($PRIMARY_BLOCK_HEX)"

DO_BLOCK_HEX=$(ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
    'curl -sf -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)

if [ -n "$DO_BLOCK_HEX" ]; then
    DO_BLOCK=$((${DO_BLOCK_HEX}))
    log "DO at block $DO_BLOCK (pre-sync, drift $((PRIMARY_BLOCK - DO_BLOCK)) blocks)"
else
    log "WARN: DO unreachable; proceeding with sync anyway"
fi

log "Step 1: Stop DO Reth (avoid mdbx concurrent-write on destination)..."
ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
    'systemctl stop reth-rope 2>/dev/null; echo STOPPED' 2>/dev/null || {
    log "ERROR: Could not stop DO Reth"
    exit 1
}
sleep 2

log "Step 2: Hot rsync data directory (BLUE stays up)..."
RSYNC_START=$(date +%s)
sudo -u ubuntu rsync -az \
    -e "ssh -o ConnectTimeout=10 -o BatchMode=yes" \
    "$DATA_DIR/" \
    "$DO_HOST:$DO_DATA/" 2>&1 | tail -1
RSYNC_ELAPSED=$(( $(date +%s) - RSYNC_START ))
log "  rsync complete in ${RSYNC_ELAPSED}s"

log "Step 3: Start DO Reth..."
ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
    'systemctl reset-failed reth-rope.service 2>/dev/null; systemctl start reth-rope.service' 2>/dev/null

sleep 12

VERIFY_HEX=$(ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
    'curl -sf -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)

if [ -n "$VERIFY_HEX" ]; then
    VERIFY_BLOCK=$((${VERIFY_HEX}))
    log "DO now at block $VERIFY_BLOCK (post-sync)"
    PRIMARY_NOW=$(curl -sf -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        http://127.0.0.1:8595 | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)
    DRIFT=$((PRIMARY_NOW - VERIFY_BLOCK))
    [ "$DRIFT" -lt 0 ] && DRIFT=$((-DRIFT))
    log "Primary now at block $PRIMARY_NOW, drift=$DRIFT blocks"

    DEPLOYER_NONCE_P=$(curl -sf -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_getTransactionCount","params":["0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195","latest"],"id":1}' \
        http://127.0.0.1:8595 | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)
    DEPLOYER_NONCE_DO=$(ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
        'curl -sf -X POST -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getTransactionCount\",\"params\":[\"0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195\",\"latest\"],\"id\":1}" \
        http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)
    if [ "$DEPLOYER_NONCE_P" = "$DEPLOYER_NONCE_DO" ]; then
        log "SUCCESS: Deployer nonce matches ($DEPLOYER_NONCE_P)"
    else
        log "WARN: Deployer nonce differs: primary=$DEPLOYER_NONCE_P do=$DEPLOYER_NONCE_DO"
    fi
else
    log "ERROR: DO not responding after restart — may need bootstrap"
    exit 1
fi

log "=== Sync complete (rsync ${RSYNC_ELAPSED}s; BLUE never stopped) ==="
