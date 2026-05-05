#!/usr/bin/env bash
# Datachain Rope Blue-Green Deployment Orchestrator
#
# Deploys dc-explorer and Reth state to BOTH VPS instances,
# ensuring they stay in sync. Adapted from tanastok-app/deploy-blue-green.sh
#
# Slots:
#   BLUE  = rope-vps  (92.243.26.189) — normally active
#   GREEN = anvil-vps (92.243.25.119) — normally standby
#
# Flow:
#   1. Sync Reth chain state (primary → secondary)
#   2. Build/deploy dc-explorer binary to secondary
#   3. Health check both nodes
#   4. Update Nginx upstream weights (or keep current active)
#
# Usage:
#   ./deploy-blue-green.sh               — sync state + deploy explorer
#   ./deploy-blue-green.sh --state-only  — sync Reth state only
#   ./deploy-blue-green.sh --failover    — force traffic to secondary

set -uo pipefail

BLUE_HOST="rope-vps"
BLUE_IP="92.243.26.189"
BLUE_SSH="ssh -p 41722 -i ~/.ssh/DCRope_key ubuntu@$BLUE_IP"
GREEN_HOST="anvil-vps"
GREEN_IP="92.243.25.119"
GREEN_SSH="ssh -i ~/.ssh/DCRope_key ubuntu@$GREEN_IP"

ACTIVE_SLOT_FILE="/opt/datachain-rope/deploy-state/active-slot"
SCRIPTS_DIR="/opt/datachain-rope/scripts"
RPC_PORT=8595
EXPLORER_PORT=3001

LOG_PREFIX="[blue-green $(date -u +%H:%M:%S)]"
log() { echo "$LOG_PREFIX $1"; }

health_check() {
    local host="$1"
    local ip="$2"

    local rpc_ok=false
    local block_hex
    block_hex=$(curl -sf --connect-timeout 3 --max-time 5 \
        -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "http://$ip:$RPC_PORT" 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)
    if [ -n "$block_hex" ] && [ "$block_hex" != "0x0" ]; then
        rpc_ok=true
    fi

    local explorer_ok=false
    local status
    status=$(curl -sf --connect-timeout 3 --max-time 5 -o /dev/null -w "%{http_code}" \
        "http://$ip:$EXPLORER_PORT/api/v1/stats" 2>/dev/null)
    if [ "$status" = "200" ]; then
        explorer_ok=true
    fi

    echo "{\"host\":\"$host\",\"ip\":\"$ip\",\"rpc\":$rpc_ok,\"block\":\"${block_hex:-0x0}\",\"explorer\":$explorer_ok}"
}

MODE="${1:-full}"

log "=== Blue-Green Deploy (mode: $MODE) ==="

if [ "$MODE" != "--failover" ]; then
    log "--- Step 1: Sync Reth State ---"
    $BLUE_SSH "bash $SCRIPTS_DIR/reth-blue-green-sync.sh" 2>&1 | while read -r line; do
        log "  [sync] $line"
    done
fi

if [ "$MODE" = "full" ] || [ "$MODE" = "--deploy-explorer" ]; then
    log "--- Step 2: Deploy dc-explorer to secondary ---"

    log "  Syncing explorer source..."
    $BLUE_SSH "rsync -az ~/datachain-rope/crates/rope-explorer/ ubuntu@$GREEN_IP:~/datachain-rope/crates/rope-explorer/" 2>/dev/null
    $BLUE_SSH "rsync -az ~/datachain-rope/Cargo.toml ~/datachain-rope/Cargo.lock ubuntu@$GREEN_IP:~/datachain-rope/" 2>/dev/null

    log "  Building on secondary (this may take several minutes)..."
    $GREEN_SSH 'export PATH="$HOME/.cargo/bin:$PATH" && cd ~/datachain-rope && cargo build --release -p rope-explorer 2>&1 | tail -3'

    log "  Restarting dc-explorer on secondary..."
    $GREEN_SSH 'sudo systemctl restart dc-explorer 2>/dev/null || echo "dc-explorer service not configured yet"'
fi

log "--- Step 3: Health Checks ---"
sleep 3

BLUE_HEALTH=$(health_check "$BLUE_HOST" "$BLUE_IP")
GREEN_HEALTH=$(health_check "$GREEN_HOST" "$GREEN_IP")

log "  BLUE:  $BLUE_HEALTH"
log "  GREEN: $GREEN_HEALTH"

BLUE_RPC=$(echo "$BLUE_HEALTH" | python3 -c "import json,sys; print(json.load(sys.stdin)['rpc'])")
GREEN_RPC=$(echo "$GREEN_HEALTH" | python3 -c "import json,sys; print(json.load(sys.stdin)['rpc'])")
BLUE_BLOCK=$(echo "$BLUE_HEALTH" | python3 -c "import json,sys; print(int(json.load(sys.stdin)['block'],16))")
GREEN_BLOCK=$(echo "$GREEN_HEALTH" | python3 -c "import json,sys; print(int(json.load(sys.stdin)['block'],16))")

if [ "$MODE" = "--failover" ]; then
    if [ "$GREEN_RPC" = "True" ]; then
        log "--- FAILOVER: Switching traffic to GREEN ($GREEN_HOST) ---"
        $BLUE_SSH "docker exec rope-nginx sed -i 's/server host.docker.internal:$RPC_PORT max_fails=2 fail_timeout=5s;/server host.docker.internal:$RPC_PORT backup;/' /etc/nginx/conf.d/datachain.network.conf && \
            docker exec rope-nginx sed -i 's/server $GREEN_IP:$RPC_PORT backup;/server $GREEN_IP:$RPC_PORT max_fails=2 fail_timeout=5s;/' /etc/nginx/conf.d/datachain.network.conf && \
            docker exec rope-nginx nginx -s reload"
        log "  Nginx now routes to GREEN"
    else
        log "  ERROR: GREEN is not healthy — cannot failover"
        exit 1
    fi
else
    DRIFT=$((BLUE_BLOCK - GREEN_BLOCK))
    if [ "$DRIFT" -lt 0 ]; then DRIFT=$((-DRIFT)); fi

    log "--- Status ---"
    log "  BLUE  (primary):   block=$BLUE_BLOCK rpc=$BLUE_RPC"
    log "  GREEN (secondary): block=$GREEN_BLOCK rpc=$GREEN_RPC"
    log "  Block drift: $DRIFT"

    if [ "$BLUE_RPC" = "True" ] && [ "$GREEN_RPC" = "True" ]; then
        log "  BOTH HEALTHY — blue-green operational"
    elif [ "$BLUE_RPC" = "True" ]; then
        log "  WARN: GREEN unhealthy — check secondary"
    elif [ "$GREEN_RPC" = "True" ]; then
        log "  WARN: BLUE unhealthy — Nginx should auto-failover to GREEN"
    else
        log "  CRITICAL: Both nodes unhealthy"
    fi
fi

log "=== Blue-Green Deploy Complete ==="
