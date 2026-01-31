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
