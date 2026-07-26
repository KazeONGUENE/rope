#!/bin/bash
# =============================================================================
# Datachain Rope - Full Deployment Script
# This script handles everything from local machine to VPS deployment
#
# SECURITY NOTE (2026-07-25 remediation, findings C2/C3 of
# docs/SECURITY_AUDIT_2026-07-25_FULL_WORKSPACE.md):
#   This script previously embedded a plaintext OpenSSH private key and a
#   live Neon Postgres OWNER connection string (plus hardcoded PG/Redis
#   passwords) directly in this file, which is tracked by git and pushed
#   to a public GitHub repository. Both keys/credentials must be treated
#   as already compromised if they were ever real — rotate them
#   independently of this fix.
#
#   This script now REQUIRES all secrets to come from the operator's local
#   environment (never embedded, never git-tracked). Run it like:
#
#     SSH_KEY="$HOME/.ssh/DCRope_key" \
#     POSTGRES_PASSWORD="$(openssl rand -base64 32)" \
#     REDIS_PASSWORD="$(openssl rand -base64 32)" \
#     NEON_DATABASE_URL="postgresql://<least-privilege-app-role>:<password>@<host>/<db>?sslmode=require" \
#     ./full-deploy.sh
#
#   The SSH private key must already exist at $SSH_KEY (generate it with
#   `ssh-keygen -t ed25519 -f ~/.ssh/DCRope_key` and register the public
#   half in the VPS's authorized_keys out-of-band) — this script will
#   refuse to run rather than fabricate or embed one.
# =============================================================================

set -euo pipefail

VPS_IP="92.243.26.189"
VPS_USER="ubuntu"
SSH_KEY="${SSH_KEY:-$HOME/.ssh/DCRope_key}"
REMOTE_DIR="/opt/datachain-rope"
LOCAL_DEPLOY_DIR="$(dirname "$0")"

: "${POSTGRES_PASSWORD:?POSTGRES_PASSWORD env var is required — generate with: openssl rand -base64 32}"
: "${REDIS_PASSWORD:?REDIS_PASSWORD env var is required — generate with: openssl rand -base64 32}"
: "${NEON_DATABASE_URL:?NEON_DATABASE_URL env var is required — use a least-privilege app role, not the *_owner role}"

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       DATACHAIN ROPE - FULL DEPLOYMENT                        ║"
echo "║       VPS: $VPS_IP                                            ║"
echo "╚════════════════════════════════════════════════════════════════╝"

# =============================================================================
# Step 1: Verify SSH Key
# =============================================================================
echo ""
echo "📝 Step 1: Verifying SSH key..."

if [ ! -f "$SSH_KEY" ]; then
    echo "❌ ERROR: SSH key not found at $SSH_KEY"
    echo "   This script never embeds or generates private key material."
    echo "   Generate one and register the public half on the VPS first:"
    echo "     ssh-keygen -t ed25519 -f \"$SSH_KEY\""
    exit 1
fi
echo "✅ SSH key found at $SSH_KEY"

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

ssh -i "$SSH_KEY" "$VPS_USER@$VPS_IP" "cat > /opt/datachain-rope/code/deploy/.env" << ENV_CREATE
# =============================================================================
# Datachain Rope - Production Environment
# Generated by full-deploy.sh from operator-supplied env vars — never
# hardcode secrets in this heredoc; see the security note at the top of
# full-deploy.sh.
# =============================================================================

# PostgreSQL (Local Docker)
POSTGRES_PASSWORD=${POSTGRES_PASSWORD}

# Redis
REDIS_PASSWORD=${REDIS_PASSWORD}

# Neon PostgreSQL (Cloud backup/sync) — must be a least-privilege app role,
# never the *_owner role.
NEON_DATABASE_URL=${NEON_DATABASE_URL}

# Node Configuration
ROPE_NODE_ID=
ROPE_CHAIN_ID=271828
ROPE_NETWORK=mainnet

# RPC Configuration
RPC_HOST=0.0.0.0
RPC_PORT=8545
WS_PORT=8546
P2P_PORT=9000

# Explorer
EXPLORER_PORT=3000
ENV_CREATE

ssh -i "$SSH_KEY" "$VPS_USER@$VPS_IP" "chmod 600 /opt/datachain-rope/code/deploy/.env"
echo "✅ .env file created (mode 600, secrets sourced from operator environment)"

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

