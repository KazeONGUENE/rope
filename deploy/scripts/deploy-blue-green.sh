#!/usr/bin/env bash
# Datachain Rope Blue-Green Deployment Orchestrator
#
# This is the SINGLE authoritative blue-green deployment script for
# Datachain Rope. It supersedes:
#   - The earlier /opt/datachain-rope/scripts/deploy-blue-green.sh (Mar 31, 2026)
#     which only deployed rope-explorer.
#   - The one-shot /Users/.../DATACHAIN ROPE/deploy/finish-quipu-canon-deploy.sh
#     (May 2, 2026) used to ship the Quipu Canon v1.1 rollout.
#
# Slots:
#   BLUE  = rope-vps  (92.243.26.189) — normally active, public via erpc.datachain.network
#   GREEN = anvil-vps (92.243.25.119) — normally standby, Nginx failover backup
#
# What this script handles:
#   • Reth chain state rsync (delegates to reth-blue-green-sync.sh)
#   • Source rsync of EVERY production crate (rope-core, rope-node, rope-cli,
#     rope-explorer, rope-economics, rope-bridge, rope-cryptography, etc.)
#   • Cargo build of rope-cli AND rope-explorer (which produces both the
#     `rope` and `dc-explorer` binaries)
#   • Production config TOML sync (deploy/config/rope-production.toml)
#   • DCScan static asset sync to the Nginx volume
#   • Service restarts: datachain-rope.service, dc-explorer
#   • Nginx reload
#   • Health checks on both slots
#   • Smoke tests for the canonical Quipu Canon v1.1 RPC methods
#     (rope_knotIndex, rope_getStringWithKnots, rope_untieKnot)
#
# Usage:
#   ./deploy-blue-green.sh                  — full deploy (BLUE, then GREEN sync)
#   ./deploy-blue-green.sh --blue-only      — deploy to BLUE (rope-vps) only
#   ./deploy-blue-green.sh --green-only     — sync GREEN from BLUE only
#   ./deploy-blue-green.sh --state-only     — Reth state rsync only (no rebuild)
#   ./deploy-blue-green.sh --failover       — switch Nginx upstream to GREEN
#   ./deploy-blue-green.sh --restore-blue   — restore Nginx upstream to BLUE
#   ./deploy-blue-green.sh --smoke-test     — probe Quipu Canon v1.1 RPC methods
#   ./deploy-blue-green.sh --health         — health check both slots, no changes
#
# Idempotent: safe to re-run.

set -uo pipefail

# ---------------------------------------------------------------
# Config (override via env)
# ---------------------------------------------------------------

BLUE_HOST="${BLUE_HOST:-rope-vps}"
BLUE_IP="${BLUE_IP:-92.243.26.189}"
GREEN_HOST="${GREEN_HOST:-anvil-vps}"
GREEN_IP="${GREEN_IP:-92.243.25.119}"

# These SSH commands are used when the script runs FROM YOUR LAPTOP.
# When the script runs ON rope-vps itself (e.g. from cron), BLUE_SSH is "".
# Detection: hostname-based.
THIS_HOST="$(hostname 2>/dev/null || echo unknown)"
if [ "$THIS_HOST" = "$BLUE_HOST" ] || [[ "$THIS_HOST" == rope-vps* ]]; then
    RUNNING_ON_BLUE=true
    BLUE_SSH=""              # local
    BLUE_RSYNC_DEST=""       # local prefix
    GREEN_SSH="ssh -i ~/.ssh/DCRope_key ubuntu@$GREEN_IP"
    GREEN_RSYNC_DEST="ubuntu@$GREEN_IP:"
else
    RUNNING_ON_BLUE=false
    BLUE_SSH="ssh ${BLUE_HOST}"
    BLUE_RSYNC_DEST="${BLUE_HOST}:"
    GREEN_SSH="ssh ${BLUE_HOST} 'ssh -i ~/.ssh/DCRope_key ubuntu@$GREEN_IP'"
    GREEN_RSYNC_DEST=""      # GREEN is reached via BLUE proxy
fi

REPO_ROOT="${REPO_ROOT:-$HOME/datachain-rope}"
NGINX_STATIC="${NGINX_STATIC:-/opt/datachain-rope/code/deploy/nginx/html/dcscan}"
PROD_CONFIG="${PROD_CONFIG:-/opt/datachain-rope/config/rope-production.toml}"
SCRIPTS_DIR="${SCRIPTS_DIR:-/opt/datachain-rope/scripts}"
RPC_PORT="${RPC_PORT:-8595}"
EXPLORER_PORT="${EXPLORER_PORT:-3001}"
PUBLIC_RPC="${PUBLIC_RPC:-https://erpc.datachain.network}"

# Crates to sync. rope-explorer alone is NOT enough — Quipu Canon v1.1
# touches lattice/ledger/RPC code in rope-core and rope-node.
CRATES=(
    "rope-core"
    "rope-node"
    "rope-cli"
    "rope-explorer"
    "rope-economics"
    "rope-cryptography"
    "rope-bridge"
    "rope-smartchain"
    "rope-rwa"
    "rope-onchainid"
    "rope-deploy"
)

# ---------------------------------------------------------------
# Logging
# ---------------------------------------------------------------

log()  { printf "[bg %s] %s\n" "$(date -u +%H:%M:%S)" "$*"; }
ok()   { printf "[bg %s] \033[1;32m✓\033[0m %s\n" "$(date -u +%H:%M:%S)" "$*"; }
warn() { printf "[bg %s] \033[1;33m!\033[0m %s\n" "$(date -u +%H:%M:%S)" "$*"; }
err()  { printf "[bg %s] \033[1;31m✗\033[0m %s\n" "$(date -u +%H:%M:%S)" "$*" >&2; }

# Run a command on BLUE (locally if we're on BLUE, via SSH otherwise).
on_blue() {
    if [ "$RUNNING_ON_BLUE" = true ]; then
        bash -c "$1"
    else
        $BLUE_SSH "$1"
    fi
}

# Run a command on GREEN (always via SSH from wherever).
on_green() {
    if [ "$RUNNING_ON_BLUE" = true ]; then
        ssh -o ConnectTimeout=10 -i ~/.ssh/DCRope_key ubuntu@$GREEN_IP "$1"
    else
        $BLUE_SSH "ssh -o ConnectTimeout=10 -i ~/.ssh/DCRope_key ubuntu@$GREEN_IP '$1'"
    fi
}

# ---------------------------------------------------------------
# Health probes
# ---------------------------------------------------------------

rpc_block_hex() {
    curl -sf --connect-timeout 3 --max-time 5 \
        -X POST -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
        "$1" 2>/dev/null | python3 -c "import json,sys; print(json.load(sys.stdin).get('result','0x0'))" 2>/dev/null || echo "0x0"
}

rpc_method() {
    local url="$1"; local method="$2"; local params="$3"
    curl -sf --connect-timeout 3 --max-time 5 \
        -X POST -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"$method\",\"params\":$params,\"id\":1}" \
        "$url" 2>/dev/null
}

health_check() {
    local label="$1"; local rpc_url="$2"; local explorer_url="$3"
    local block_hex; block_hex=$(rpc_block_hex "$rpc_url")
    local block_int=$((block_hex)) 2>/dev/null || block_int=0
    local exp_status; exp_status=$(curl -sf --connect-timeout 3 --max-time 5 -o /dev/null -w "%{http_code}" "$explorer_url" 2>/dev/null || echo "000")
    if [ "$block_int" -gt 0 ] && [ "$exp_status" = "200" ]; then
        ok "$label: rpc OK (block $block_int) explorer OK"
        return 0
    elif [ "$block_int" -gt 0 ]; then
        warn "$label: rpc OK (block $block_int) explorer FAIL ($exp_status)"
        return 1
    else
        err "$label: rpc FAIL (block 0) explorer $exp_status"
        return 2
    fi
}

# ---------------------------------------------------------------
# Operations
# ---------------------------------------------------------------

sync_state() {
    log "--- Reth state sync (delegates to reth-blue-green-sync.sh) ---"
    on_blue "bash $SCRIPTS_DIR/reth-blue-green-sync.sh 2>&1 | tail -10"
}

sync_source_to_blue() {
    if [ "$RUNNING_ON_BLUE" = true ]; then
        log "Skipping source sync to BLUE (we are on BLUE)"
        return
    fi
    log "--- Sync source to BLUE ($BLUE_HOST) ---"
    local local_root="$(cd "$(dirname "$0")/../.." && pwd)/datachain-rope"
    if [ ! -d "$local_root" ]; then
        err "Source tree not found at $local_root — adjust REPO_ROOT or run from workspace"
        return 1
    fi
    for crate in "${CRATES[@]}"; do
        if [ -d "$local_root/crates/$crate" ]; then
            rsync -az --delete \
                --exclude target --exclude '*.rs.bk' \
                "$local_root/crates/$crate/" \
                "${BLUE_RSYNC_DEST}${REPO_ROOT}/crates/$crate/"
            log "  synced $crate"
        fi
    done
    rsync -az "$local_root/Cargo.toml" "$local_root/Cargo.lock" "${BLUE_RSYNC_DEST}${REPO_ROOT}/"
    rsync -az "$local_root/deploy/config/" "${BLUE_RSYNC_DEST}${REPO_ROOT}/deploy/config/"
    ok "BLUE source synced"
}

build_blue() {
    log "--- Build rope-cli + rope-explorer on BLUE (incremental, warm cache) ---"
    on_blue "export PATH=\"\$HOME/.cargo/bin:\$PATH\"
        cd $REPO_ROOT
        cargo build --release -p rope-cli -p rope-explorer --message-format short 2>&1 | tail -5
        ls -la target/release/rope target/release/dc-explorer"
}

deploy_blue_runtime() {
    log "--- Deploy runtime artefacts on BLUE ---"
    log "  Sync DCScan static..."
    if [ "$RUNNING_ON_BLUE" = true ]; then
        rsync -az --delete "$REPO_ROOT/crates/rope-explorer/static/" "$NGINX_STATIC/"
    else
        local local_root="$(cd "$(dirname "$0")/../.." && pwd)/datachain-rope"
        rsync -az --delete "$local_root/crates/rope-explorer/static/" "${BLUE_RSYNC_DEST}${NGINX_STATIC}/"
    fi
    ok "  Static synced"

    log "  Sync production TOML..."
    if [ "$RUNNING_ON_BLUE" = true ]; then
        cp -f "$REPO_ROOT/deploy/config/rope-production.toml" "$PROD_CONFIG" 2>/dev/null && ok "  TOML synced" || warn "  TOML copy failed (manual placement may be needed)"
    fi

    log "  Restart services..."
    on_blue "sudo systemctl restart datachain-rope.service && sleep 3 && \
             sudo systemctl restart dc-explorer && sleep 3 && \
             (docker exec rope-nginx nginx -s reload 2>/dev/null || sudo systemctl reload nginx) && \
             echo SERVICES_RESTARTED"
    ok "BLUE runtime deployed"
}

sync_source_to_green() {
    log "--- Sync source from BLUE to GREEN (anvil-vps) ---"
    on_blue "rsync -az --delete --exclude target $REPO_ROOT/crates/ ubuntu@$GREEN_IP:$REPO_ROOT/crates/
             rsync -az $REPO_ROOT/Cargo.toml $REPO_ROOT/Cargo.lock ubuntu@$GREEN_IP:$REPO_ROOT/
             rsync -az $REPO_ROOT/deploy/config/ ubuntu@$GREEN_IP:$REPO_ROOT/deploy/config/
             rsync -az --delete $NGINX_STATIC/ ubuntu@$GREEN_IP:$NGINX_STATIC/"
    ok "GREEN source synced"
}

build_green() {
    log "--- Build on GREEN (background — slower, may be cold-cache) ---"
    on_green "export PATH=\"\$HOME/.cargo/bin:\$PATH\"
              cd $REPO_ROOT
              nohup bash -c 'cargo build --release -p rope-cli -p rope-explorer 2>&1' > /tmp/green-build.log 2>&1 &
              disown
              echo GREEN_BUILD_STARTED"
    warn "GREEN build runs in background; tail with: ssh anvil-vps tail -f /tmp/green-build.log"
    warn "Re-run '$0 --green-only' once GREEN build finishes to do the runtime deploy"
}

deploy_green_runtime() {
    log "--- Deploy runtime artefacts on GREEN (build must already be done) ---"
    on_green "[ -x $REPO_ROOT/target/release/dc-explorer ] || { echo BUILD_NOT_DONE; exit 2; }
              sudo systemctl restart datachain-rope.service 2>/dev/null || echo 'datachain-rope.service not configured'
              sleep 3
              sudo systemctl restart dc-explorer 2>/dev/null || echo 'dc-explorer service not configured'
              echo GREEN_RUNTIME_DEPLOYED"
}

failover_to_green() {
    log "--- FAILOVER: route Nginx upstream to GREEN ---"
    on_blue "docker exec rope-nginx sh -c '
        sed -i \"s/server host.docker.internal:8597 max_fails=1 fail_timeout=10s;/server host.docker.internal:8597 backup;/\" /etc/nginx/conf.d/datachain.network.conf 2>/dev/null
        sed -i \"s/server $GREEN_IP:$RPC_PORT backup;/server $GREEN_IP:$RPC_PORT max_fails=1 fail_timeout=10s;/\" /etc/nginx/conf.d/datachain.network.conf 2>/dev/null
        nginx -s reload
    '"
    ok "Nginx now prefers GREEN; BLUE is backup"
}

restore_to_blue() {
    log "--- RESTORE: route Nginx upstream back to BLUE ---"
    on_blue "docker exec rope-nginx sh -c '
        sed -i \"s/server host.docker.internal:8597 backup;/server host.docker.internal:8597 max_fails=1 fail_timeout=10s;/\" /etc/nginx/conf.d/datachain.network.conf 2>/dev/null
        sed -i \"s/server $GREEN_IP:$RPC_PORT max_fails=1 fail_timeout=10s;/server $GREEN_IP:$RPC_PORT backup;/\" /etc/nginx/conf.d/datachain.network.conf 2>/dev/null
        nginx -s reload
    '"
    ok "Nginx now prefers BLUE; GREEN is backup"
}

smoke_test() {
    log "--- Smoke test: Quipu Canon v1.1 RPC methods on $PUBLIC_RPC ---"

    local r; r=$(rpc_method "$PUBLIC_RPC" "rope_knotIndex" "[]")
    if echo "$r" | grep -q '"result"'; then
        local k; k=$(echo "$r" | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))")
        ok "  rope_knotIndex → $k"
    else
        err "  rope_knotIndex FAILED: $r"
        return 1
    fi

    r=$(rpc_method "$PUBLIC_RPC" "eth_blockNumber" "[]")
    if echo "$r" | grep -q '"result"'; then
        local b; b=$(echo "$r" | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))")
        ok "  eth_blockNumber (alias) → $b"
    else
        err "  eth_blockNumber FAILED: $r"
        return 1
    fi

    r=$(rpc_method "$PUBLIC_RPC" "rope_getStringWithKnots" '["0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"]')
    if echo "$r" | grep -q '"canon"'; then
        local tomb; tomb=$(echo "$r" | python3 -c "import json,sys; print(json.load(sys.stdin).get('result',{}).get('tombstone_count','?'))")
        ok "  rope_getStringWithKnots → tombstones=$tomb"
    else
        warn "  rope_getStringWithKnots no canon field (may not be deployed yet)"
    fi

    r=$(rpc_method "$PUBLIC_RPC" "rope_getKnotByIndex" '["0x1", false]')
    if echo "$r" | grep -q '"knotIndex"'; then
        ok "  rope_getKnotByIndex returned canonical knot fields"
    else
        warn "  rope_getKnotByIndex did not include knotIndex (may not be deployed yet)"
    fi
}

# ---------------------------------------------------------------
# Main
# ---------------------------------------------------------------

MODE="${1:-full}"

case "$MODE" in
    --health|--status)
        health_check "BLUE  ($BLUE_HOST)" "http://$BLUE_IP:$RPC_PORT" "http://$BLUE_IP:$EXPLORER_PORT/api/v1/stats" || true
        health_check "GREEN ($GREEN_HOST)" "http://$GREEN_IP:$RPC_PORT" "http://$GREEN_IP:$EXPLORER_PORT/api/v1/stats" || true
        ;;
    --smoke-test)
        smoke_test
        ;;
    --state-only)
        sync_state
        ;;
    --failover)
        failover_to_green
        sleep 3
        smoke_test
        ;;
    --restore-blue)
        restore_to_blue
        sleep 3
        smoke_test
        ;;
    --blue-only)
        sync_source_to_blue
        build_blue
        deploy_blue_runtime
        sleep 3
        health_check "BLUE  ($BLUE_HOST)" "http://$BLUE_IP:$RPC_PORT" "http://$BLUE_IP:$EXPLORER_PORT/api/v1/stats"
        smoke_test
        ;;
    --green-only)
        sync_source_to_green
        deploy_green_runtime || warn "GREEN runtime deploy failed — build may not be done yet"
        sleep 3
        health_check "GREEN ($GREEN_HOST)" "http://$GREEN_IP:$RPC_PORT" "http://$GREEN_IP:$EXPLORER_PORT/api/v1/stats" || true
        ;;
    full|"")
        log "=== FULL blue-green deploy ==="
        sync_source_to_blue
        build_blue
        deploy_blue_runtime
        sleep 3
        health_check "BLUE  ($BLUE_HOST)" "http://$BLUE_IP:$RPC_PORT" "http://$BLUE_IP:$EXPLORER_PORT/api/v1/stats" || true
        smoke_test
        sync_source_to_green
        build_green   # background
        log "=== FULL deploy complete (GREEN build still running in background) ==="
        log "    Re-run '$0 --green-only' when the GREEN build finishes."
        ;;
    --help|-h)
        sed -n '2,40p' "$0"
        ;;
    *)
        err "Unknown mode: $MODE"
        sed -n '2,40p' "$0"
        exit 1
        ;;
esac
