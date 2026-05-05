#!/usr/bin/env bash
# Reth Blue-Green State Sync — Primary → Secondary
#
# Directly rsyncs the Reth data directory from primary to secondary.
# The secondary is stopped during transfer to avoid corruption, then restarted.
#
# Architecture:
#   1. Record primary block number
#   2. Stop secondary Reth
#   3. rsync primary data dir → secondary data dir (compressed, incremental)
#   4. Start secondary Reth
#   5. Verify both chains report similar block numbers
#
# The Reth data dir is ~585MB. Over the inter-VPS link (~100MB/s),
# incremental rsync takes <10 seconds after the initial full copy.
#
# Run: every 15 minutes via cron
# Cron: */15 * * * * /opt/datachain-rope/scripts/reth-blue-green-sync.sh >> /var/log/reth-sync.log 2>&1

set -uo pipefail

LOCK_FILE="/tmp/reth-sync.lock"
DATA_DIR="/opt/datachain-rope/reth/data"
SECONDARY_HOST="ubuntu@92.243.25.119"
SECONDARY_DATA="/opt/datachain-rope/reth/data"
LOG_PREFIX="[reth-sync $(date -u +%H:%M:%S)]"

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

log "=== Starting blue-green sync ==="

PRIMARY_BLOCK_HEX=$(curl -sf -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://127.0.0.1:8595 | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)

if [ -z "$PRIMARY_BLOCK_HEX" ]; then
    log "ERROR: Primary RPC not responding, aborting"
    exit 1
fi

PRIMARY_BLOCK=$((${PRIMARY_BLOCK_HEX}))
log "Primary at block $PRIMARY_BLOCK ($PRIMARY_BLOCK_HEX)"

SECONDARY_BLOCK_HEX=$(ssh -o ConnectTimeout=5 -o BatchMode=yes "$SECONDARY_HOST" \
    'curl -sf -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)

if [ -n "$SECONDARY_BLOCK_HEX" ]; then
    SECONDARY_BLOCK=$((${SECONDARY_BLOCK_HEX}))
    log "Secondary at block $SECONDARY_BLOCK (pre-sync)"
else
    SECONDARY_BLOCK=0
    log "Secondary unreachable or not running (pre-sync)"
fi

log "Step 1: Stopping secondary Reth..."
ssh -o ConnectTimeout=5 -o BatchMode=yes "$SECONDARY_HOST" \
    'sudo systemctl stop reth-rope 2>/dev/null; echo STOPPED' 2>/dev/null || {
    log "ERROR: Could not reach secondary"
    exit 1
}

sleep 2

log "Step 2: rsync data directory (primary → secondary)..."
RSYNC_START=$(date +%s)

rsync -az --delete --info=progress2 \
    -e "ssh -o ConnectTimeout=10" \
    "$DATA_DIR/" \
    "$SECONDARY_HOST:$SECONDARY_DATA/" 2>&1 | tail -1

RSYNC_ELAPSED=$(( $(date +%s) - RSYNC_START ))
log "rsync complete in ${RSYNC_ELAPSED}s"

log "Step 3: Starting secondary Reth..."
ssh -o ConnectTimeout=5 -o BatchMode=yes "$SECONDARY_HOST" \
    'sudo systemctl start reth-rope' 2>/dev/null

sleep 8

VERIFY_HEX=$(ssh -o ConnectTimeout=5 -o BatchMode=yes "$SECONDARY_HOST" \
    'curl -sf -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)

if [ -n "$VERIFY_HEX" ]; then
    VERIFY_BLOCK=$((${VERIFY_HEX}))
    log "Secondary now at block $VERIFY_BLOCK (post-sync)"

    PRIMARY_NOW=$(curl -sf -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        http://127.0.0.1:8595 | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)

    DRIFT=$((PRIMARY_NOW - VERIFY_BLOCK))
    if [ "$DRIFT" -lt 0 ]; then DRIFT=$((-DRIFT)); fi

    log "Primary now at block $PRIMARY_NOW, drift=$DRIFT blocks"

    DEPLOYER_NONCE_P=$(curl -sf -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_getTransactionCount","params":["0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195","latest"],"id":1}' \
        http://127.0.0.1:8595 | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)

    DEPLOYER_NONCE_S=$(ssh -o ConnectTimeout=5 -o BatchMode=yes "$SECONDARY_HOST" \
        'curl -sf -X POST -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getTransactionCount\",\"params\":[\"0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195\",\"latest\"],\"id\":1}" \
        http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)

    if [ "$DEPLOYER_NONCE_P" = "$DEPLOYER_NONCE_S" ]; then
        log "SUCCESS: Deployer nonce matches ($DEPLOYER_NONCE_P) — chains are in sync"
    else
        log "WARN: Deployer nonce mismatch: primary=$DEPLOYER_NONCE_P secondary=$DEPLOYER_NONCE_S"
    fi
else
    log "ERROR: Secondary not responding after restart"
fi

log "=== Sync complete (${RSYNC_ELAPSED}s transfer) ==="
