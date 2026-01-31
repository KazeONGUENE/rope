#!/bin/bash
# Datachain Rope Bootstrap Node Deployment Script
# Target: VPS 92.243.26.189

set -e

VPS_IP="92.243.26.189"
VPS_USER="root"
DEPLOY_DIR="/opt/datachain-rope"
DATA_DIR="/var/lib/datachain-rope"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║     DATACHAIN ROPE BOOTSTRAP NODE DEPLOYMENT                 ║"
echo "╚══════════════════════════════════════════════════════════════╝"
echo ""

# Check if binary exists
if [ ! -f "./target/release/rope" ]; then
    echo "Error: rope binary not found. Run 'cargo build --release' first."
    exit 1
fi

# Check if bootstrap keys exist
if [ ! -f "./bootstrap-keys/node.key" ]; then
    echo "Error: Bootstrap keys not found. Run 'rope keygen' first."
    exit 1
fi

echo "1. Building release binary..."
cargo build --release

echo ""
echo "2. Preparing deployment package..."
mkdir -p deploy/package
cp ./target/release/rope deploy/package/
cp ./bootstrap-keys/node.key deploy/package/
cp ./bootstrap-keys/node.pub deploy/package/
cp ./config/networks/testnet.json deploy/package/

# Create systemd service file
cat > deploy/package/datachain-rope.service << 'SERVICE'
[Unit]
Description=Datachain Rope Bootstrap Node
After=network.target

[Service]
Type=simple
User=datachain
Group=datachain
WorkingDirectory=/opt/datachain-rope
ExecStart=/opt/datachain-rope/rope node --network testnet --mode validator --data-dir /var/lib/datachain-rope
Restart=always
RestartSec=10
LimitNOFILE=65535
Environment="RUST_LOG=info,rope_network=debug"

# Resource limits
MemoryLimit=4G
CPUQuota=200%

[Install]
WantedBy=multi-user.target
SERVICE

# Create installation script
cat > deploy/package/install.sh << 'INSTALL'
#!/bin/bash
set -e

echo "Installing Datachain Rope Bootstrap Node..."

# Create user
if ! id "datachain" &>/dev/null; then
    useradd -r -s /bin/false datachain
fi

# Create directories
mkdir -p /opt/datachain-rope
mkdir -p /var/lib/datachain-rope/keys
mkdir -p /var/log/datachain-rope

# Copy files
cp rope /opt/datachain-rope/
cp testnet.json /opt/datachain-rope/
cp node.key /var/lib/datachain-rope/keys/
cp node.pub /var/lib/datachain-rope/keys/

# Set permissions
chmod +x /opt/datachain-rope/rope
chown -R datachain:datachain /opt/datachain-rope
chown -R datachain:datachain /var/lib/datachain-rope
chown -R datachain:datachain /var/log/datachain-rope

# Install systemd service
cp datachain-rope.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable datachain-rope

echo ""
echo "Installation complete!"
echo "Start with: systemctl start datachain-rope"
echo "Logs: journalctl -u datachain-rope -f"
INSTALL

chmod +x deploy/package/install.sh

echo ""
echo "3. Deployment package ready in deploy/package/"
echo ""
echo "To deploy to VPS, run:"
echo "  scp -r deploy/package/* ${VPS_USER}@${VPS_IP}:/tmp/rope-deploy/"
echo "  ssh ${VPS_USER}@${VPS_IP} 'cd /tmp/rope-deploy && bash install.sh'"
echo ""
echo "Or deploy manually:"
echo "  1. Copy files to VPS"
echo "  2. Run install.sh as root"
echo "  3. Start service: systemctl start datachain-rope"
echo ""
echo "Bootstrap Node Details:"
echo "  Peer ID: 12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM"
echo "  Multiaddr: /ip4/92.243.26.189/tcp/9000/p2p/12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM"
echo ""
