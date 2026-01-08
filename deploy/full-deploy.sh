#!/bin/bash
# =============================================================================
# Datachain Rope - Full Deployment Script
# This script handles everything from local machine to VPS deployment
# =============================================================================

set -e

VPS_IP="92.243.26.189"
VPS_USER="ubuntu"
SSH_KEY="$HOME/.ssh/DCRope_key"
REMOTE_DIR="/opt/datachain-rope"
LOCAL_DEPLOY_DIR="$(dirname "$0")"

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       DATACHAIN ROPE - FULL DEPLOYMENT                        ║"
echo "║       VPS: $VPS_IP                                            ║"
echo "╚════════════════════════════════════════════════════════════════╝"

# =============================================================================
# Step 1: Save SSH Key
# =============================================================================
echo ""
echo "📝 Step 1: Setting up SSH key..."

if [ ! -f "$SSH_KEY" ]; then
    echo "Creating SSH key file..."
    cat > "$SSH_KEY" << 'SSHKEY'
-----BEGIN OPENSSH PRIVATE KEY-----
b3BlbnNzaC1rZXktdjEAAAAABG5vbmUAAAAEbm9uZQAAAAAAAAABAAABFwAAAAdzc2gtcn
NhAAAAAwEAAQAAAQEAmpc5kH+ARkEFRThAEb8IZ0sgc60NKWXcEcm5Y3wnbNfZg9iZ4KEQ
0JcukOM759+vskNKUB2Mpoyve1ABgnbrop1DkVZnjzBZgUthssfkWJxOUrNV0FWqr9MHoc
pYUI77QGhH/VlLd5AoEEQS3CPLyL+zIg6hWtWIY1HC+K7iZ4E56rV43iOf33RZsUUUvHII
r6yJA3QRdxwjrUOztIWkG+6mXMDceXSaili6noroDH3HzJdKwZiB4b87T3MeRJgMupEkQQ
dGztNYD27/o0nnin8FbheUqfcMaSXOJZNvXHVugGD0H+FaGaWFw2qwIxPSNucMyNldLNOh
3EENSJKZ0wAAA9g/LqY5Py6mOQAAAAdzc2gtcnNhAAABAQCalzmQf4BGQQVFOEARvwhnSy
BzrQ0pZdwRybljfCds19mD2JngoRDQly6Q4zvn36+yQ0pQHYymjK97UAGCduuinUORVmeP
MFmBS2Gyx+RYnE5Ss1XQVaqv0wehylhQjvtAaEf9WUt3kCgQRBLcI8vIv7MiDqFa1YhjUc
L4ruJngTnqtXjeI5/fdFmxRRS8cgivrIkDdBF3HCOtQ7O0haQb7qZcwNx5dJqKWLqeiugM
fcfMl0rBmIHhvztPcx5EmAy6kSRBB0bO01gPbv+jSeeKfwVuF5Sp9wxpJc4lk29cdW6AYP
Qf4VoZpYXDarAjE9I25wzI2V0s06HcQQ1IkpnTAAAAAwEAAQAAAQAWrzqGY9HzZ9wkSvuf
1Fv2farek+tlV+nsqaDCIqL4Zk4n+j348o7wmkFTh9IFFakHAWJ4vMqkWpWaLznMJrOEx7
X1Wuv/B4BtfXMcfxeYvHWueLFkIltVyfjfOr2DM2VWugFXrGWWFGRvSptH4XAoXkpPej0e
1/cCYqo3wBXp9m1FjTlDGrTdStxRv60FU69Lk50pEvk80yKdzCJ18wMur07GpQAsS+kFkp
Ui5HQog+MSmzxdxGdSi5B/OlRD7MTNpNgQnc8CVhh//xHkEOB7hzWkiNzAE5KvMt/LR/fN
40+dV0ZwgkgLQbI/15YKqi5kHAassM4M+UF92W8ijzkZAAAAgFXw4cd6tSU/1SzLHDc53/
ze4BkrhUi9IMEc9XcrMEO++rEPpB97L9kSC/mcrcQrf3xpQypNXAmdR1POHL7qYGoCYq7S
0PTx4rxao7dZZihsxCw2AX27lC1LtgQI+7zrqrhV2zO83kQdwV9xt0P0iff6wGNyRQ2vO4
fu5r/pTgDkAAAAgQDNXq1aFEz44Bg4NY1sr/dqQMNfJpOyL4MTb7GRMa6rN8dtq67d22CZ
KkkKxDDoBD74WsgxAF7MWm0/UVhLj5vnRCGFVEKvzlSdrW3lypSZ9G8lST7a2CwWxSfldX
EnUZo2fiRW9MyKUbu93E1OSmYBqZUgHfBykmwrZjogHyNkuwAAAIEAwLPFY4+yY0tFCPRY
lJXEH06VTwYTgVzyMpYmqt/LAwzlNrtgz/eIS5HuWUSEwB9073ACJAaMzQ6+W44VZ/VflE
iy7skwcj6ng6Uusxb5X5+uFkBd+Td1kahVccyMV5DRf3vaZqEyemxDV/pf0MPlaLVmC8s+
qpH5ptp3XFzS2ckAAAAhR2VuZXJhdGVkIGJ5IFNob3dETlMgLSAyMDI1LTEwLTA4AQI=
-----END OPENSSH PRIVATE KEY-----
SSHKEY
    chmod 600 "$SSH_KEY"
    echo "✅ SSH key created at $SSH_KEY"
else
    echo "✅ SSH key already exists"
fi

# =============================================================================
# Step 2: Test Connection
# =============================================================================
echo ""
echo "🔌 Step 2: Testing SSH connection..."
ssh -i "$SSH_KEY" -o ConnectTimeout=10 -o StrictHostKeyChecking=no "$VPS_USER@$VPS_IP" "echo 'SSH connection successful!'" || {
    echo "❌ ERROR: Cannot connect to VPS"
    echo "Please check:"
    echo "  - VPS is running"
    echo "  - SSH key is correct"
    echo "  - Firewall allows SSH"
    exit 1
}

# =============================================================================
# Step 3: Initial VPS Setup
# =============================================================================
echo ""
echo "🖥️ Step 3: Running initial VPS setup..."

ssh -i "$SSH_KEY" "$VPS_USER@$VPS_IP" << 'REMOTE_SETUP'
set -e

echo "📦 Updating system..."
sudo apt update && sudo apt upgrade -y

echo "📦 Installing dependencies..."
sudo apt install -y curl wget git build-essential pkg-config libssl-dev clang htop tmux fail2ban ufw

# Install Docker if not present
if ! command -v docker &> /dev/null; then
    echo "🐳 Installing Docker..."
    curl -fsSL https://get.docker.com | sudo sh
    sudo usermod -aG docker $USER
fi

# Install Docker Compose if not present
if ! command -v docker-compose &> /dev/null; then
    echo "🐳 Installing Docker Compose..."
    sudo curl -L "https://github.com/docker/compose/releases/latest/download/docker-compose-$(uname -s)-$(uname -m)" -o /usr/local/bin/docker-compose
    sudo chmod +x /usr/local/bin/docker-compose
fi

# Configure Firewall
echo "🔥 Configuring firewall..."
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow ssh
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw allow 9000/tcp
yes | sudo ufw enable || true

# Create directories
echo "📁 Creating directories..."
sudo mkdir -p /opt/datachain-rope/{code,ssl,data,logs}
sudo chown -R $USER:$USER /opt/datachain-rope

echo "✅ VPS setup complete!"
REMOTE_SETUP

# =============================================================================
# Step 4: Upload Files
# =============================================================================
echo ""
echo "📤 Step 4: Uploading deployment files..."

# Upload deploy directory
rsync -avz -e "ssh -i $SSH_KEY" \
    --exclude '*.pem' \
    --exclude '.git' \
    "$LOCAL_DEPLOY_DIR/" "$VPS_USER@$VPS_IP:$REMOTE_DIR/code/deploy/"

# =============================================================================
# Step 5: Install SSL Certificates
# =============================================================================
echo ""
echo "🔐 Step 5: Installing SSL certificates..."

ssh -i "$SSH_KEY" "$VPS_USER@$VPS_IP" "chmod +x $REMOTE_DIR/code/deploy/install-ssl-certs.sh && $REMOTE_DIR/code/deploy/install-ssl-certs.sh"

# =============================================================================
# Step 6: Create .env file
# =============================================================================
echo ""
echo "⚙️ Step 6: Creating .env file..."

ssh -i "$SSH_KEY" "$VPS_USER@$VPS_IP" << 'ENV_CREATE'
cat > /opt/datachain-rope/code/deploy/.env << 'ENVFILE'
# =============================================================================
# Datachain Rope - Production Environment
# =============================================================================

# PostgreSQL (Local Docker)
POSTGRES_PASSWORD=DCRope_Secure_PG_2026!

# Redis
REDIS_PASSWORD=DCRope_Redis_Cache_2026!

# Neon PostgreSQL (Cloud backup/sync)
NEON_DATABASE_URL=postgresql://neondb_owner:npg_Gr7mLYdpaI9S@ep-noisy-sun-a9xwa3gc-pooler.gwc.azure.neon.tech/neondb?sslmode=require&channel_binding=require

# Node Configuration
ROPE_NODE_ID=
ROPE_CHAIN_ID=314159
ROPE_NETWORK=mainnet

# RPC Configuration
RPC_HOST=0.0.0.0
RPC_PORT=8545
WS_PORT=8546
P2P_PORT=9000

# Explorer
EXPLORER_PORT=3000
ENVFILE

echo "✅ .env file created"
ENV_CREATE

# =============================================================================
# Step 7: Start Services
# =============================================================================
echo ""
echo "🚀 Step 7: Starting services..."

ssh -i "$SSH_KEY" "$VPS_USER@$VPS_IP" << 'START_SERVICES'
cd /opt/datachain-rope/code/deploy

# Make scripts executable
chmod +x *.sh

# Start with docker-compose
echo "Starting containers..."
docker-compose up -d || {
    echo "Docker compose failed. Checking if user needs to re-login for docker group..."
    echo "Please log out and log back in, then run: cd /opt/datachain-rope/code/deploy && docker-compose up -d"
}

# Wait for services
sleep 10

# Check status
echo ""
echo "Container status:"
docker ps

echo ""
echo "✅ Deployment complete!"
START_SERVICES

echo ""
echo "╔════════════════════════════════════════════════════════════════╗"
echo "║  🎉 DEPLOYMENT COMPLETE!                                       ║"
echo "║                                                                ║"
echo "║  Services:                                                     ║"
echo "║  • Main:     https://datachain.network                        ║"
echo "║  • RPC:      https://erpc.datachain.network                   ║"
echo "║  • WS:       wss://ws.datachain.network                       ║"
echo "║  • Explorer: https://dcscan.io                                ║"
echo "║                                                                ║"
echo "║  To check logs:                                               ║"
echo "║  ssh -i ~/.ssh/DCRope_key ubuntu@92.243.26.189               ║"
echo "║  cd /opt/datachain-rope/code/deploy && docker-compose logs -f ║"
echo "╚════════════════════════════════════════════════════════════════╝"

