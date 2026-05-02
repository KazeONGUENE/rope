#!/usr/bin/env bash
# IPFS Setup on Secondary VPS (anvil-vps / 92.243.25.119)
#
# Installs Kubo IPFS, configures peering with primary and DCSwap,
# and sets up the systemd service. Run from the PRIMARY VPS via SSH.
#
# Usage: ssh rope-vps 'bash /opt/datachain-rope/scripts/ipfs-setup-secondary.sh'

set -euo pipefail

SECONDARY_HOST="ubuntu@92.243.25.119"
PRIMARY_PEER_ID="12D3KooWHxKVdUKHSwzSw8epVP4yWXcNqG7VixvVRnTfnXXxbqhM"
DCSWAP_PEER_ID="12D3KooWJB8MgSzXd17C3FDRTK8jFg71LaNaL8myNK5AwRn8FG6Z"

echo "=== Setting up IPFS on secondary VPS ==="

ssh -o BatchMode=yes "$SECONDARY_HOST" bash -s << 'REMOTE_SCRIPT'
set -euo pipefail

IPFS_PATH=/opt/datachain-rope/ipfs
export IPFS_PATH

if command -v ipfs &>/dev/null; then
    echo "IPFS already installed: $(ipfs version)"
else
    echo "Installing Kubo IPFS..."
    KUBO_VERSION="v0.33.2"
    cd /tmp
    wget -q "https://dist.ipfs.tech/kubo/${KUBO_VERSION}/kubo_${KUBO_VERSION}_linux-amd64.tar.gz"
    tar -xzf "kubo_${KUBO_VERSION}_linux-amd64.tar.gz"
    sudo install kubo/ipfs /usr/local/bin/ipfs
    rm -rf kubo "kubo_${KUBO_VERSION}_linux-amd64.tar.gz"
    echo "Installed: $(ipfs version)"
fi

if [ ! -f "$IPFS_PATH/config" ]; then
    echo "Initializing IPFS repo at $IPFS_PATH..."
    sudo mkdir -p "$IPFS_PATH"
    sudo chown -R ubuntu:ubuntu "$IPFS_PATH"
    ipfs init --profile server
    echo "IPFS repo initialized"
else
    echo "IPFS repo already exists at $IPFS_PATH"
fi

echo "Configuring IPFS..."
ipfs config Addresses.API "/ip4/127.0.0.1/tcp/5001"
ipfs config Addresses.Gateway "/ip4/127.0.0.1/tcp/8080"
ipfs config Datastore.StorageMax "10GB"
ipfs config --json Peering.Peers "[
    {\"ID\": \"PRIMARYPEER\", \"Addrs\": [\"/ip4/92.243.26.189/tcp/4001\"]},
    {\"ID\": \"DCSWAPPEER\", \"Addrs\": [\"/ip4/92.243.26.114/tcp/4001\"]}
]"

sed -i "s/PRIMARYPEER/12D3KooWHxKVdUKHSwzSw8epVP4yWXcNqG7VixvVRnTfnXXxbqhM/" "$IPFS_PATH/config"
sed -i "s/DCSWAPPEER/12D3KooWJB8MgSzXd17C3FDRTK8jFg71LaNaL8myNK5AwRn8FG6Z/" "$IPFS_PATH/config"

echo "Installing systemd service..."
sudo tee /etc/systemd/system/ipfs.service > /dev/null << 'SVCEOF'
[Unit]
Description=IPFS Daemon (Kubo) - Datachain Rope Secondary
After=network.target

[Service]
Type=notify
User=ubuntu
Group=ubuntu
Environment=IPFS_PATH=/opt/datachain-rope/ipfs
ExecStart=/usr/local/bin/ipfs daemon --enable-gc
Restart=on-failure
RestartSec=10
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
SVCEOF

sudo systemctl daemon-reload
sudo systemctl enable ipfs
sudo systemctl start ipfs

sleep 5
echo ""
echo "=== IPFS Status ==="
systemctl status ipfs --no-pager | head -8
echo ""
echo "Peer ID: $(ipfs id -f '<id>')"
echo "Peers: $(ipfs swarm peers | wc -l)"
echo ""

echo "Connecting to primary..."
ipfs swarm connect "/ip4/92.243.26.189/tcp/4001/p2p/12D3KooWHxKVdUKHSwzSw8epVP4yWXcNqG7VixvVRnTfnXXxbqhM" 2>/dev/null && \
    echo "Connected to primary IPFS" || echo "Primary IPFS connection failed"

echo "Connecting to DCSwap..."
ipfs swarm connect "/ip4/92.243.26.114/tcp/4001/p2p/12D3KooWJB8MgSzXd17C3FDRTK8jFg71LaNaL8myNK5AwRn8FG6Z" 2>/dev/null && \
    echo "Connected to DCSwap IPFS" || echo "DCSwap IPFS connection failed"

echo ""
echo "=== IPFS Setup Complete on Secondary ==="
REMOTE_SCRIPT

echo ""
echo "Registering secondary as peer on primary..."
SECONDARY_PEER_ID=$(ssh -o BatchMode=yes "$SECONDARY_HOST" \
    "IPFS_PATH=/opt/datachain-rope/ipfs ipfs id -f '<id>'" 2>/dev/null)

if [ -n "$SECONDARY_PEER_ID" ]; then
    export IPFS_PATH=/opt/datachain-rope/ipfs
    CURRENT_PEERS=$(ipfs config Peering.Peers 2>/dev/null)

    if echo "$CURRENT_PEERS" | grep -q "$SECONDARY_PEER_ID"; then
        echo "Secondary already in primary's peering config"
    else
        ipfs config --json Peering.Peers "$(echo "$CURRENT_PEERS" | python3 -c "
import json, sys
peers = json.load(sys.stdin)
peers.append({'ID': '$SECONDARY_PEER_ID', 'Addrs': ['/ip4/92.243.25.119/tcp/4001']})
print(json.dumps(peers))
")"
        echo "Added secondary ($SECONDARY_PEER_ID) to primary peering config"
    fi

    ipfs swarm connect "/ip4/92.243.25.119/tcp/4001/p2p/$SECONDARY_PEER_ID" 2>/dev/null && \
        echo "Connected to secondary IPFS from primary" || echo "Connection to secondary failed"
fi

echo ""
echo "=== Three-Node IPFS Mesh ==="
echo "  Primary (rope-vps):    $PRIMARY_PEER_ID"
echo "  Secondary (anvil-vps): ${SECONDARY_PEER_ID:-unknown}"
echo "  DCSwap:                $DCSWAP_PEER_ID"
