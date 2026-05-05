#!/usr/bin/env bash
# Reth Health Check — Monitors both primary and secondary nodes
#
# Checks:
#   1. Reth RPC responsive (eth_blockNumber)
#   2. Block production advancing (not stuck)
#   3. DC Explorer API responsive
#   4. Cross-VPS connectivity
#
# If primary is unhealthy and secondary is healthy, logs a FAILOVER alert.
# Nginx upstream handles automatic failover — this script is for monitoring/alerting.
#
# Run: every 2 minutes via cron
# Cron: */2 * * * * /opt/datachain-rope/scripts/reth-health-check.sh >> /var/log/reth-health.log 2>&1

set -uo pipefail

PRIMARY_RPC="http://127.0.0.1:8595"
SECONDARY_HOST="ubuntu@92.243.25.119"
SECONDARY_RPC="http://92.243.25.119:8595"
EXPLORER_URL="http://127.0.0.1:3001/api/v1/stats"
STATE_FILE="/tmp/reth-health-state.json"
LOG_PREFIX="[health $(date -u +%H:%M:%S)]"

log() { echo "$LOG_PREFIX $1"; }

rpc_block() {
    local url="$1"
    local result
    result=$(curl -sf --connect-timeout 3 --max-time 5 \
        -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$url" 2>/dev/null)
    if [ $? -eq 0 ] && [ -n "$result" ]; then
        echo "$result" | python3 -c "import json,sys; print(json.load(sys.stdin).get('result','0x0'))" 2>/dev/null
    else
        echo "0x0"
    fi
}

PRIMARY_HEX=$(rpc_block "$PRIMARY_RPC")
PRIMARY_BLOCK=$((${PRIMARY_HEX}))

SECONDARY_HEX=$(rpc_block "$SECONDARY_RPC")
SECONDARY_BLOCK=$((${SECONDARY_HEX}))

PREV_PRIMARY=0
if [ -f "$STATE_FILE" ]; then
    PREV_PRIMARY=$(python3 -c "
import json
with open('$STATE_FILE') as f: d = json.load(f)
print(d.get('primary_block', 0))
" 2>/dev/null || echo "0")
fi

PRIMARY_ADVANCING=false
if [ "$PRIMARY_BLOCK" -gt "$PREV_PRIMARY" ] && [ "$PRIMARY_BLOCK" -gt 0 ]; then
    PRIMARY_ADVANCING=true
fi

EXPLORER_OK=false
EXPLORER_STATUS=$(curl -sf --connect-timeout 3 --max-time 5 -o /dev/null -w "%{http_code}" "$EXPLORER_URL" 2>/dev/null)
if [ "$EXPLORER_STATUS" = "200" ]; then
    EXPLORER_OK=true
fi

PRIMARY_HEALTHY=false
if [ "$PRIMARY_BLOCK" -gt 0 ]; then
    PRIMARY_HEALTHY=true
fi

SECONDARY_HEALTHY=false
if [ "$SECONDARY_BLOCK" -gt 0 ]; then
    SECONDARY_HEALTHY=true
fi

DRIFT=$((PRIMARY_BLOCK - SECONDARY_BLOCK))
if [ "$DRIFT" -lt 0 ]; then DRIFT=$((-DRIFT)); fi

python3 -c "
import json, datetime
state = {
    'timestamp': datetime.datetime.utcnow().strftime('%Y-%m-%dT%H:%M:%SZ'),
    'primary_block': $PRIMARY_BLOCK,
    'primary_hex': '$PRIMARY_HEX',
    'primary_healthy': $( [ "$PRIMARY_HEALTHY" = true ] && echo "True" || echo "False" ),
    'primary_advancing': $( [ "$PRIMARY_ADVANCING" = true ] && echo "True" || echo "False" ),
    'secondary_block': $SECONDARY_BLOCK,
    'secondary_hex': '$SECONDARY_HEX',
    'secondary_healthy': $( [ "$SECONDARY_HEALTHY" = true ] && echo "True" || echo "False" ),
    'block_drift': $DRIFT,
    'explorer_ok': $( [ "$EXPLORER_OK" = true ] && echo "True" || echo "False" ),
    'prev_primary_block': $PREV_PRIMARY
}
with open('$STATE_FILE', 'w') as f:
    json.dump(state, f, indent=2)
"

if [ "$PRIMARY_HEALTHY" = true ]; then
    if [ "$PRIMARY_ADVANCING" = true ]; then
        log "OK primary=$PRIMARY_BLOCK (+$(( PRIMARY_BLOCK - PREV_PRIMARY ))) secondary=$SECONDARY_BLOCK drift=$DRIFT explorer=$EXPLORER_STATUS"
    else
        log "WARN primary=$PRIMARY_BLOCK (STALLED, prev=$PREV_PRIMARY) secondary=$SECONDARY_BLOCK drift=$DRIFT"
    fi
else
    if [ "$SECONDARY_HEALTHY" = true ]; then
        log "FAILOVER primary=DOWN secondary=$SECONDARY_BLOCK — Nginx should auto-failover"
    else
        log "CRITICAL primary=DOWN secondary=DOWN — both nodes unresponsive"
    fi
fi
