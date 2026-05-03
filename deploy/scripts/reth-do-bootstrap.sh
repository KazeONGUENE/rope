#!/usr/bin/env bash
# Reth Blue-DO Bootstrap — used only when DO mdbx is corrupt or empty.
# Briefly stops BLUE, streams a clean tar to DO, restarts both.
#
# Usage:
#   reth-do-bootstrap.sh                       # rpc-1 (157.230.18.45)
#   reth-do-bootstrap.sh 167.172.106.174       # rpc-2 (or any DO host)
#
# IMPORTANT: For DO→DO bootstrap (e.g. rpc-1 → rpc-2), prefer running the
# tar-stream directly between the two DO hosts (much faster, no BLUE downtime).
# This script always streams from the LOCAL host (BLUE), so only use it from BLUE.

set -uo pipefail
DO_TARGET="${1:-${DO_TARGET:-157.230.18.45}}"
DATA_DIR="/opt/datachain-rope/reth"
DO_HOST="root@${DO_TARGET}"
DO_DATA="/opt/datachain-rope/reth"
LOG_PREFIX="[reth-do-bootstrap ${DO_TARGET} $(date -u +%H:%M:%S)]"
log() { echo "$LOG_PREFIX $1"; }

log "=== Bootstrapping DO from clean BLUE snapshot ==="
GREEN_OK=$(ssh -o ConnectTimeout=5 -o BatchMode=yes ubuntu@92.243.25.119 \
    'curl -sf --max-time 3 -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null)
[ -z "$GREEN_OK" ] && { log "ERROR: GREEN unhealthy — refusing"; exit 1; }
log "GREEN healthy — safe to briefly stop BLUE"

ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
    'systemctl stop reth-rope; rm -rf /opt/datachain-rope/reth/data/*; mkdir -p /opt/datachain-rope/reth/data; chown -R ubuntu:ubuntu /opt/datachain-rope/reth/data'

sudo systemctl stop reth-rope
STOP=$(date -u +%s)
log "BLUE stopped, streaming..."
sudo tar -C "$DATA_DIR" -cf - data \
    | ssh -o ConnectTimeout=10 -o BatchMode=yes "$DO_HOST" "tar -C $DO_DATA -xf -"
sudo systemctl start reth-rope
DOWN=$(($(date -u +%s) - STOP))
log "BLUE restarted (was down ${DOWN}s)"

ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
    'chown -R ubuntu:ubuntu /opt/datachain-rope/reth/data; systemctl reset-failed reth-rope.service; systemctl start reth-rope.service'
sleep 12
log "Verify DO:"
ssh -o ConnectTimeout=8 -o BatchMode=yes "$DO_HOST" \
    'curl -sf -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595'
log "=== bootstrap done ==="
