#!/bin/bash
# =============================================================================
# recover-vps.sh — Datachain Rope VPS Recovery
# =============================================================================
#
# Run from local machine after VPS comes back online.
# This script:
#   1. Verifies SSH access
#   2. Checks Anvil state (contracts intact vs. wiped)
#   3. Installs the persistent anvil-rope.service (--state flag)
#   4. Restarts all services
#   5. If Anvil was reset: runs T-REX redeployment
#   6. Reports final nonce for DCSwap handoff
#
# Usage:
#   ./recover-vps.sh              # full recovery
#   ./recover-vps.sh --check-only # just check status, no changes
# =============================================================================

set -euo pipefail

VPS_IP="92.243.26.189"
VPS_USER="ubuntu"
SSH_KEY="$HOME/.ssh/DCRope_key"
RPC_URL="https://erpc.datachain.network"
RPC_LOCAL="http://127.0.0.1:8545"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEPLOY_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
PROJECT_ROOT="$(cd "$DEPLOY_DIR/.." && pwd)"
WORKSPACE_ROOT="$(cd "$PROJECT_ROOT/.." && pwd)"

CHECK_ONLY=false
[[ "${1:-}" == "--check-only" ]] && CHECK_ONLY=true

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

log()   { echo -e "${BLUE}[RECOVER]${NC} $1"; }
ok()    { echo -e "${GREEN}  [OK]${NC} $1"; }
warn()  { echo -e "${YELLOW}  [WARN]${NC} $1"; }
fail()  { echo -e "${RED}  [FAIL]${NC} $1"; }

SSH_CMD="ssh -i $SSH_KEY -o ConnectTimeout=15 -o StrictHostKeyChecking=no $VPS_USER@$VPS_IP"

# Known contract addresses from the Feb 26 deployment
FACTORY_ADDR="0x8B3554e7D32dEeB8A8c057268E1Eebd6c043313C"
IDENTITY_IMPL_ADDR="0xe158A7b8030Af5386AAE3baE4fc7382200064f20"

echo ""
echo "================================================================"
echo "  DATACHAIN ROPE — VPS RECOVERY"
echo "  Target: $VPS_USER@$VPS_IP"
echo "  Mode: $([ "$CHECK_ONLY" = true ] && echo 'CHECK ONLY' || echo 'FULL RECOVERY')"
echo "================================================================"
echo ""

# ─────────────────────────────────────────────────────────────────────
# Step 1: Verify SSH
# ─────────────────────────────────────────────────────────────────────
log "Step 1: Testing SSH connectivity..."

if ! $SSH_CMD "echo 'SSH_OK'" 2>/dev/null | grep -q SSH_OK; then
    fail "Cannot reach VPS via SSH. Is it online?"
    echo ""
    echo "  Try:  ping $VPS_IP"
    echo "  Try:  ssh -i $SSH_KEY $VPS_USER@$VPS_IP"
    exit 1
fi
ok "SSH connection established"

# ─────────────────────────────────────────────────────────────────────
# Step 2: Check VPS service status
# ─────────────────────────────────────────────────────────────────────
log "Step 2: Checking VPS services..."

$SSH_CMD << 'STATUS_CHECK'
echo "--- systemd services ---"
for svc in anvil-rope datachain-rope ipfs; do
    STATUS=$(systemctl is-active $svc 2>/dev/null || echo "not-found")
    echo "  $svc: $STATUS"
done

echo ""
echo "--- docker containers ---"
docker ps --format "  {{.Names}}: {{.Status}}" 2>/dev/null || echo "  (docker not running or not installed)"

echo ""
echo "--- ports in use ---"
ss -tlnp 2>/dev/null | grep -E ':(8545|8546|80|443|3001|4001|5001|8080) ' || echo "  (no relevant ports found)"

echo ""
echo "--- anvil binary ---"
if [ -x /home/ubuntu/.foundry/bin/anvil ]; then
    echo "  Anvil binary: FOUND at /home/ubuntu/.foundry/bin/anvil"
    /home/ubuntu/.foundry/bin/anvil --version 2>/dev/null || echo "  (version check failed)"
else
    echo "  Anvil binary: NOT FOUND"
fi

echo ""
echo "--- state file ---"
if [ -f /opt/datachain-rope/anvil-state/state.json ]; then
    SIZE=$(stat -c%s /opt/datachain-rope/anvil-state/state.json 2>/dev/null || stat -f%z /opt/datachain-rope/anvil-state/state.json 2>/dev/null)
    echo "  State file: EXISTS ($SIZE bytes)"
else
    echo "  State file: NOT FOUND"
fi
STATUS_CHECK

if [ "$CHECK_ONLY" = true ]; then
    echo ""
    log "Check complete (--check-only). Exiting."
    exit 0
fi

# ─────────────────────────────────────────────────────────────────────
# Step 3: Install persistent anvil-rope.service
# ─────────────────────────────────────────────────────────────────────
log "Step 3: Installing persistent anvil-rope.service..."

scp -i "$SSH_KEY" -o StrictHostKeyChecking=no \
    "$DEPLOY_DIR/anvil-rope.service" \
    "$VPS_USER@$VPS_IP:/tmp/anvil-rope.service"

$SSH_CMD << 'INSTALL_SERVICE'
set -e

# Stop conflicting services
sudo systemctl stop anvil-rope 2>/dev/null || true
sudo systemctl stop datachain-rope 2>/dev/null || true
sudo systemctl disable datachain-rope 2>/dev/null || true

# Install the new service
sudo cp /tmp/anvil-rope.service /etc/systemd/system/anvil-rope.service
sudo systemctl daemon-reload
sudo systemctl enable anvil-rope

# Ensure state directory exists
sudo mkdir -p /opt/datachain-rope/anvil-state
sudo chown -R ubuntu:ubuntu /opt/datachain-rope/anvil-state

# Start Anvil
echo "Starting Anvil with state persistence..."
sudo systemctl start anvil-rope
sleep 4

# Verify it's running
if systemctl is-active --quiet anvil-rope; then
    echo "  anvil-rope.service: ACTIVE"
else
    echo "  anvil-rope.service: FAILED"
    journalctl -u anvil-rope --no-pager -n 20
    exit 1
fi
INSTALL_SERVICE

ok "anvil-rope.service installed with --state persistence"

# ─────────────────────────────────────────────────────────────────────
# Step 4: Check if chain state was preserved
# ─────────────────────────────────────────────────────────────────────
log "Step 4: Checking on-chain state..."

CHAIN_STATE=$($SSH_CMD << 'CHECK_CHAIN'
RPC="http://127.0.0.1:8545"

BLOCK_NUM=$(curl -s -m 5 -X POST "$RPC" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
echo "BLOCK_NUMBER=$BLOCK_NUM"

CHAIN_ID=$(curl -s -m 5 -X POST "$RPC" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \
    | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
echo "CHAIN_ID=$CHAIN_ID"

# Check DCSwap Factory bytecode
FACTORY_CODE=$(curl -s -m 5 -X POST "$RPC" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_getCode","params":["0x8B3554e7D32dEeB8A8c057268E1Eebd6c043313C","latest"],"id":1}' \
    | grep -o '"result":"[^"]*"' | cut -d'"' -f4)

# Check T-REX IdentityImplementation bytecode
IDENTITY_CODE=$(curl -s -m 5 -X POST "$RPC" \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_getCode","params":["0xe158A7b8030Af5386AAE3baE4fc7382200064f20","latest"],"id":1}' \
    | grep -o '"result":"[^"]*"' | cut -d'"' -f4)

if [ -n "$FACTORY_CODE" ] && [ "$FACTORY_CODE" != "0x" ] && [ ${#FACTORY_CODE} -gt 10 ]; then
    echo "DCSWAP_FACTORY=INTACT"
else
    echo "DCSWAP_FACTORY=WIPED"
fi

if [ -n "$IDENTITY_CODE" ] && [ "$IDENTITY_CODE" != "0x" ] && [ ${#IDENTITY_CODE} -gt 10 ]; then
    echo "TREX_IDENTITY=INTACT"
else
    echo "TREX_IDENTITY=WIPED"
fi
CHECK_CHAIN
)

echo "$CHAIN_STATE"

FACTORY_STATUS=$(echo "$CHAIN_STATE" | grep "DCSWAP_FACTORY" | cut -d= -f2)
TREX_STATUS=$(echo "$CHAIN_STATE" | grep "TREX_IDENTITY" | cut -d= -f2)

if [ "$FACTORY_STATUS" = "INTACT" ] && [ "$TREX_STATUS" = "INTACT" ]; then
    ok "Chain state preserved. No redeployment needed."
    echo ""
    echo "================================================================"
    echo "  RECOVERY COMPLETE — STATE PRESERVED"
    echo ""
    echo "  Anvil is running with --state persistence."
    echo "  All contracts intact. Restart Docker services if needed:"
    echo ""
    echo "    ssh -i $SSH_KEY $VPS_USER@$VPS_IP"
    echo "    cd /opt/datachain-rope/code/deploy"
    echo "    docker-compose up -d"
    echo "================================================================"
    exit 0
fi

# ─────────────────────────────────────────────────────────────────────
# Step 5: Anvil was reset — redeploy T-REX
# ─────────────────────────────────────────────────────────────────────
warn "Chain state was WIPED. Redeployment required."
echo ""
log "Step 5: Redeploying T-REX infrastructure..."

# Check if node_modules exist locally
if [ ! -d "$WORKSPACE_ROOT/node_modules/@onchain-id" ]; then
    warn "Missing @onchain-id/solidity. Running npm install..."
    (cd "$WORKSPACE_ROOT" && npm install)
fi

if [ ! -d "$WORKSPACE_ROOT/node_modules/@tokenysolutions" ]; then
    warn "Missing @tokenysolutions/t-rex. Running npm install..."
    (cd "$WORKSPACE_ROOT" && npm install)
fi

# Fund deployer from genesis account first
log "Funding deployer from genesis account..."
DEPLOYER="0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"

if command -v cast &> /dev/null; then
    cast rpc anvil_impersonateAccount "0x302fa11a6e784dfa89f96942a919c09b45559676" --rpc-url "$RPC_URL" 2>/dev/null || true
    cast send --unlocked --from "0x302fa11a6e784dfa89f96942a919c09b45559676" \
        "$DEPLOYER" --value 500000000ether --rpc-url "$RPC_URL" 2>/dev/null || warn "cast send failed — deployer may already have funds"
    cast rpc anvil_stopImpersonatingAccount "0x302fa11a6e784dfa89f96942a919c09b45559676" --rpc-url "$RPC_URL" 2>/dev/null || true
    ok "Deployer funded"
else
    warn "Foundry 'cast' not found locally. Fund deployer manually before proceeding."
fi

# Run T-REX deployment
log "Running T-REX deployment script..."
echo "  Script: $WORKSPACE_ROOT/deploy-scripts/deploy_trex_infra_and_register_tanastok.js"
echo ""

(cd "$WORKSPACE_ROOT" && RPC_URL="$RPC_URL" node deploy-scripts/deploy_trex_infra_and_register_tanastok.js)

TREX_EXIT=$?
if [ $TREX_EXIT -ne 0 ]; then
    fail "T-REX deployment failed (exit code $TREX_EXIT)"
    exit 1
fi
ok "T-REX infrastructure deployed successfully"

# ─────────────────────────────────────────────────────────────────────
# Step 6: Report nonce for DCSwap handoff
# ─────────────────────────────────────────────────────────────────────
log "Step 6: Reporting deployer nonce for DCSwap handoff..."

if command -v cast &> /dev/null; then
    NONCE=$(cast nonce "$DEPLOYER" --rpc-url "$RPC_URL" 2>/dev/null || echo "UNKNOWN")
else
    NONCE="UNKNOWN (install Foundry to check)"
fi

echo ""
echo "================================================================"
echo "  RECOVERY COMPLETE — REDEPLOYMENT DONE"
echo ""
echo "  Anvil: RUNNING with --state persistence"
echo "  T-REX: DEPLOYED"
echo "  Deployer nonce: $NONCE"
echo ""
echo "  NEXT: Notify DCSwap to redeploy at nonce $NONCE+"
echo ""
echo "  T-REX addresses saved to:"
echo "    $WORKSPACE_ROOT/deployed_trex_addresses.json"
echo ""
echo "  Docker services may need restart:"
echo "    ssh -i $SSH_KEY $VPS_USER@$VPS_IP"
echo "    cd /opt/datachain-rope/code/deploy"
echo "    docker-compose up -d"
echo "================================================================"
