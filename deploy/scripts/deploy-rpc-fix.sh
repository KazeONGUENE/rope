#!/bin/bash
# deploy-rpc-fix.sh - Deploy RPC and WebSocket fixes to Datachain Rope nodes
# 
# This script:
# 1. Builds the rope-node with WebSocket support
# 2. Deploys updated nginx configs
# 3. Restarts services on RPC nodes

set -e

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

# Configuration
RPC_NODES=(
    "157.230.18.45"    # datachain-rpc-1
    "167.172.106.174"  # datachain-rpc-2
)

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
DEPLOY_USER="${DEPLOY_USER:-root}"
SSH_KEY="${SSH_KEY:-}"

echo -e "${GREEN}╔═══════════════════════════════════════════════════════════╗${NC}"
echo -e "${GREEN}║   Datachain Rope - RPC & WebSocket Fix Deployment        ║${NC}"
echo -e "${GREEN}╚═══════════════════════════════════════════════════════════╝${NC}"
echo ""

# Build rope-node
echo -e "${YELLOW}[1/4] Building rope-node with WebSocket support...${NC}"
cd "$PROJECT_ROOT"
cargo build --release -p rope-node 2>&1 | tail -5

if [ ! -f "target/release/rope-node" ]; then
    echo -e "${RED}Build failed! rope-node binary not found${NC}"
    exit 1
fi

echo -e "${GREEN}✓ Build successful${NC}"

# Deploy to each RPC node
for node_ip in "${RPC_NODES[@]}"; do
    echo ""
    echo -e "${YELLOW}[2/4] Deploying to $node_ip...${NC}"
    
    SSH_OPTS=""
    if [ -n "$SSH_KEY" ]; then
        SSH_OPTS="-i $SSH_KEY"
    fi
    
    # Test connection
    if ! ssh $SSH_OPTS -o ConnectTimeout=5 "$DEPLOY_USER@$node_ip" "echo 'Connected'" 2>/dev/null; then
        echo -e "${RED}Cannot connect to $node_ip - skipping${NC}"
        continue
    fi
    
    echo "  Uploading rope-node binary..."
    scp $SSH_OPTS "$PROJECT_ROOT/target/release/rope-node" "$DEPLOY_USER@$node_ip:/tmp/rope-node"
    
    echo "  Uploading nginx configs..."
    scp $SSH_OPTS "$PROJECT_ROOT/deploy/nginx/conf.d/datachain.network.conf" "$DEPLOY_USER@$node_ip:/tmp/datachain.network.conf"
    scp $SSH_OPTS "$PROJECT_ROOT/deploy/nginx/conf.d/rope.network.conf" "$DEPLOY_USER@$node_ip:/tmp/rope.network.conf"
    
    echo "  Installing and restarting services..."
    ssh $SSH_OPTS "$DEPLOY_USER@$node_ip" bash << 'REMOTE_SCRIPT'
        set -e
        
        # Stop rope-node service
        echo "    Stopping rope-node..."
        systemctl stop rope-node 2>/dev/null || true
        
        # Backup and replace binary
        if [ -f /opt/rope/bin/rope-node ]; then
            cp /opt/rope/bin/rope-node /opt/rope/bin/rope-node.bak
        fi
        mv /tmp/rope-node /opt/rope/bin/rope-node
        chmod +x /opt/rope/bin/rope-node
        
        # Update nginx configs
        if [ -d /etc/nginx/conf.d ]; then
            mv /tmp/datachain.network.conf /etc/nginx/conf.d/datachain.network.conf
            mv /tmp/rope.network.conf /etc/nginx/conf.d/rope.network.conf
            
            # Test nginx config
            echo "    Testing nginx config..."
            nginx -t
            
            # Reload nginx
            echo "    Reloading nginx..."
            systemctl reload nginx
        fi
        
        # Start rope-node service
        echo "    Starting rope-node..."
        systemctl start rope-node
        
        # Wait for startup
        sleep 3
        
        # Health check
        echo "    Running health check..."
        CHAIN_ID=$(curl -s -m 5 -X POST http://localhost:8545 \
            -H "Content-Type: application/json" \
            -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \
            | grep -o '"result":"[^"]*"' | cut -d'"' -f4)
        
        if [ "$CHAIN_ID" = "0x425d4" ]; then
            echo "    ✓ HTTP RPC working (chain_id: $CHAIN_ID)"
        else
            echo "    ✗ HTTP RPC check failed (got: $CHAIN_ID)"
            exit 1
        fi
        
        # Check WebSocket port
        if nc -z localhost 8546 2>/dev/null; then
            echo "    ✓ WebSocket port 8546 is open"
        else
            echo "    ✗ WebSocket port 8546 not responding"
        fi
        
        echo "    Deployment complete!"
REMOTE_SCRIPT
    
    echo -e "${GREEN}✓ Deployed to $node_ip${NC}"
done

# Verify public endpoints
echo ""
echo -e "${YELLOW}[3/4] Verifying public endpoints...${NC}"

endpoints=(
    "https://erpc.datachain.network"
    "https://erpc.rope.network"
)

for endpoint in "${endpoints[@]}"; do
    echo -n "  Testing $endpoint... "
    result=$(curl -s -m 10 -X POST "$endpoint" \
        -H "Content-Type: application/json" \
        -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' 2>/dev/null)
    
    if echo "$result" | grep -q "0x425d4"; then
        echo -e "${GREEN}✓ OK (271828)${NC}"
    else
        echo -e "${RED}✗ Failed${NC}"
        echo "    Response: $result"
    fi
done

# Summary
echo ""
echo -e "${YELLOW}[4/4] Deployment Summary${NC}"
echo "═══════════════════════════════════════════════════════════"
echo ""
echo "  Changes deployed:"
echo "    • rope-node with WebSocket support (port 8546)"
echo "    • nginx config: erpc.rope.network now proxies to RPC"
echo "    • nginx config: ws.datachain.network WebSocket ready"
echo ""
echo "  Endpoints:"
echo "    • HTTP RPC: https://erpc.datachain.network"
echo "    • HTTP RPC: https://erpc.rope.network"
echo "    • WebSocket: wss://ws.datachain.network"
echo ""
echo -e "${GREEN}Deployment complete!${NC}"
echo ""
echo "Run ChainList health check in ~5 minutes to verify scores."
