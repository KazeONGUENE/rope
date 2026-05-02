#!/usr/bin/env bash
# Datachain Rope Infrastructure Setup — Full Orchestrator
#
# Deploys all scripts, sets up cron jobs, installs IPFS on secondary,
# runs initial IPFS pins, and verifies blue-green readiness.
#
# Run from PRIMARY VPS: bash /opt/datachain-rope/scripts/setup-all.sh

set -uo pipefail

SCRIPTS_DIR="/opt/datachain-rope/scripts"
IPFS_DATA="/opt/datachain-rope/ipfs-data"

export IPFS_PATH=/opt/datachain-rope/ipfs

mkdir -p "$SCRIPTS_DIR" "$IPFS_DATA" "/opt/datachain-rope/deploy-state"

echo "============================================"
echo " Datachain Rope — Infrastructure Setup"
echo " $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "============================================"
echo ""

# --- 1. IPFS on Secondary ---
echo ">>> [1/6] Setting up IPFS on secondary VPS..."
if ssh -o ConnectTimeout=3 -o BatchMode=yes ubuntu@92.243.25.119 \
    'IPFS_PATH=/opt/datachain-rope/ipfs ipfs id -f "<id>" 2>/dev/null' >/dev/null 2>&1; then
    echo "  IPFS already running on secondary"
else
    bash "$SCRIPTS_DIR/ipfs-setup-secondary.sh"
fi
echo ""

# --- 2. Pin Genesis + Chain State ---
echo ">>> [2/6] Pinning Reth chain state to IPFS..."
bash "$SCRIPTS_DIR/ipfs-pin-reth-state.sh"
echo ""

# --- 3. Pin Contracts ---
echo ">>> [3/6] Pinning contract deployments to IPFS..."
bash "$SCRIPTS_DIR/ipfs-pin-contracts.sh"
echo ""

# --- 4. Cross-Pin + Storacha ---
echo ">>> [4/6] Cross-pinning to peers + Storacha..."
bash "$SCRIPTS_DIR/ipfs-crosspin-storacha.sh"
echo ""

# --- 5. Initial Blue-Green Sync ---
echo ">>> [5/6] Running initial blue-green state sync..."
bash "$SCRIPTS_DIR/reth-blue-green-sync.sh"
echo ""

# --- 6. Setup Cron Jobs ---
echo ">>> [6/6] Installing cron jobs..."

EXISTING_CRON=$(crontab -l 2>/dev/null || echo "")
NEW_CRON="$EXISTING_CRON"

add_cron() {
    local schedule="$1"
    local cmd="$2"
    local comment="$3"
    if ! echo "$NEW_CRON" | grep -qF "$cmd"; then
        NEW_CRON="$NEW_CRON
# $comment
$schedule $cmd >> /var/log/datachain-rope-\$(basename $cmd .sh).log 2>&1"
        echo "  Added: $comment ($schedule)"
    else
        echo "  Exists: $comment"
    fi
}

add_cron "*/15 * * * *" "$SCRIPTS_DIR/reth-blue-green-sync.sh" "Blue-green Reth state sync (every 15 min)"
add_cron "*/2 * * * *"  "$SCRIPTS_DIR/reth-health-check.sh" "Health check both nodes (every 2 min)"
add_cron "0 */6 * * *"  "$SCRIPTS_DIR/ipfs-pin-reth-state.sh" "IPFS pin Reth state (every 6 hours)"
add_cron "0 3 * * 1"    "$SCRIPTS_DIR/ipfs-pin-contracts.sh" "IPFS pin contracts (weekly Monday)"
add_cron "0 4 * * 1"    "$SCRIPTS_DIR/ipfs-crosspin-storacha.sh" "Cross-pin + Storacha (weekly Monday)"

echo "$NEW_CRON" | crontab -
echo ""

# --- Verification ---
echo "============================================"
echo " Verification"
echo "============================================"
echo ""

echo "--- Reth Primary ---"
P_BLOCK=$(curl -sf -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    http://127.0.0.1:8595 | python3 -c "import json,sys; r=json.load(sys.stdin)['result']; print(f'{int(r,16)} ({r})')")
echo "  Block: $P_BLOCK"

echo "--- Reth Secondary ---"
S_BLOCK=$(ssh -o ConnectTimeout=3 -o BatchMode=yes ubuntu@92.243.25.119 \
    'curl -sf -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
    http://127.0.0.1:8595' 2>/dev/null | python3 -c "import json,sys; r=json.load(sys.stdin)['result']; print(f'{int(r,16)} ({r})')" 2>/dev/null)
echo "  Block: ${S_BLOCK:-UNREACHABLE}"

echo ""
echo "--- IPFS Primary ---"
echo "  Repo: $(ipfs repo stat 2>/dev/null | grep RepoSize)"
echo "  Pins: $(ipfs pin ls --type=recursive 2>/dev/null | wc -l) recursive"
echo "  Peers: $(ipfs swarm peers 2>/dev/null | wc -l)"

echo ""
echo "--- IPFS Secondary ---"
S_IPFS=$(ssh -o ConnectTimeout=3 -o BatchMode=yes ubuntu@92.243.25.119 \
    'IPFS_PATH=/opt/datachain-rope/ipfs ipfs repo stat 2>/dev/null | grep RepoSize' 2>/dev/null)
echo "  Repo: ${S_IPFS:-NOT RUNNING}"

echo ""
echo "--- Cron Schedule ---"
crontab -l 2>/dev/null | grep -v '^$' | grep datachain-rope

echo ""
echo "--- DC Explorer ---"
systemctl status dc-explorer --no-pager 2>/dev/null | head -3

echo ""
echo "============================================"
echo " Setup Complete!"
echo "============================================"
echo ""
echo " Primary:    92.243.26.189 (rope-vps)"
echo " Secondary:  92.243.25.119 (anvil-vps)"
echo " RPC:        https://erpc.datachain.network"
echo " Explorer:   https://dcscan.io"
echo " IPFS Data:  $IPFS_DATA"
echo " Logs:       /var/log/datachain-rope-*.log"
