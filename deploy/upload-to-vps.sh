#!/bin/bash
# =============================================================================
# Datachain Rope - Upload to VPS Script
# Run this from your LOCAL machine to deploy files to VPS
# =============================================================================

VPS_IP="92.243.26.189"
VPS_USER="ubuntu"
SSH_KEY="~/.ssh/DCRope_key"
REMOTE_DIR="/opt/datachain-rope"

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       DATACHAIN ROPE - UPLOAD TO VPS                          ║"
echo "║       Target: $VPS_IP                                         ║"
echo "╚════════════════════════════════════════════════════════════════╝"

# Check SSH key
if [ ! -f $(eval echo $SSH_KEY) ]; then
    echo "❌ ERROR: SSH key not found at $SSH_KEY"
    exit 1
fi

# Test connection
echo "🔌 Testing SSH connection..."
ssh -i $SSH_KEY -o ConnectTimeout=10 $VPS_USER@$VPS_IP "echo 'Connection successful!'" || {
    echo "❌ ERROR: Cannot connect to VPS"
    exit 1
}

# Create directories on VPS
echo "📁 Creating directories on VPS..."
ssh -i $SSH_KEY $VPS_USER@$VPS_IP "sudo mkdir -p $REMOTE_DIR/{code,ssl,data,logs}"
ssh -i $SSH_KEY $VPS_USER@$VPS_IP "sudo chown -R $VPS_USER:$VPS_USER $REMOTE_DIR"

# Upload deploy folder
echo "📤 Uploading deployment files..."
rsync -avz -e "ssh -i $SSH_KEY" \
    --exclude 'ssl-certs/*.pem' \
    --exclude '*.log' \
    --exclude '.git' \
    ./ $VPS_USER@$VPS_IP:$REMOTE_DIR/code/deploy/

# Upload config
echo "📤 Uploading configuration..."
rsync -avz -e "ssh -i $SSH_KEY" \
    ../config/ $VPS_USER@$VPS_IP:$REMOTE_DIR/code/config/

# Set permissions
echo "🔒 Setting permissions..."
ssh -i $SSH_KEY $VPS_USER@$VPS_IP "chmod +x $REMOTE_DIR/code/deploy/*.sh"

echo ""
echo "╔════════════════════════════════════════════════════════════════╗"
echo "║  ✅ UPLOAD COMPLETE!                                           ║"
echo "║                                                                ║"
echo "║  Next steps (run on VPS):                                     ║"
echo "║  1. ssh -i $SSH_KEY $VPS_USER@$VPS_IP                         ║"
echo "║  2. cd $REMOTE_DIR/code/deploy                                 ║"
echo "║  3. ./setup-vps.sh (first time only)                          ║"
echo "║  4. Upload SSL certificates                                   ║"
echo "║  5. Configure .env file                                       ║"
echo "║  6. ./deploy.sh                                               ║"
echo "╚════════════════════════════════════════════════════════════════╝"

