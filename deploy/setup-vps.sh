#!/bin/bash
# =============================================================================
# Datachain Rope VPS Setup Script
# VPS: 92.243.26.189
# Run this ONCE after first SSH connection
# =============================================================================

set -e

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       DATACHAIN ROPE - VPS PRODUCTION SETUP                    ║"
echo "║       VPS: 92.243.26.189                                       ║"
echo "╚════════════════════════════════════════════════════════════════╝"

# Update system
echo "📦 Updating system packages..."
sudo apt update && sudo apt upgrade -y

# Install dependencies
echo "📦 Installing dependencies..."
sudo apt install -y \
    curl \
    wget \
    git \
    build-essential \
    pkg-config \
    libssl-dev \
    clang \
    htop \
    tmux \
    fail2ban \
    ufw

# Install Docker
echo "🐳 Installing Docker..."
curl -fsSL https://get.docker.com | sudo sh
sudo usermod -aG docker $USER

# Install Docker Compose
echo "🐳 Installing Docker Compose..."
sudo curl -L "https://github.com/docker/compose/releases/latest/download/docker-compose-$(uname -s)-$(uname -m)" -o /usr/local/bin/docker-compose
sudo chmod +x /usr/local/bin/docker-compose

# Install Rust
echo "🦀 Installing Rust..."
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
source $HOME/.cargo/env

# Configure Firewall
echo "🔥 Configuring firewall..."
sudo ufw default deny incoming
sudo ufw default allow outgoing
sudo ufw allow ssh
sudo ufw allow 80/tcp    # HTTP
sudo ufw allow 443/tcp   # HTTPS
sudo ufw allow 9000/tcp  # P2P
sudo ufw --force enable

# Configure fail2ban
echo "🔒 Configuring fail2ban..."
sudo systemctl enable fail2ban
sudo systemctl start fail2ban

# Create directories
echo "📁 Creating directories..."
sudo mkdir -p /opt/datachain-rope
sudo mkdir -p /opt/datachain-rope/ssl
sudo mkdir -p /opt/datachain-rope/data
sudo mkdir -p /opt/datachain-rope/logs
sudo chown -R $USER:$USER /opt/datachain-rope

# Clone repository
echo "📥 Cloning Datachain Rope..."
cd /opt/datachain-rope
git clone https://github.com/KazeONGUENE/rope.git code
cd code

# Build the project
echo "🔨 Building Datachain Rope..."
cargo build --release

echo ""
echo "╔════════════════════════════════════════════════════════════════╗"
echo "║  ✅ SETUP COMPLETE!                                            ║"
echo "║                                                                ║"
echo "║  Next steps:                                                   ║"
echo "║  1. Log out and log back in (for Docker group)                ║"
echo "║  2. Copy SSL certificates to /opt/datachain-rope/ssl/         ║"
echo "║  3. Copy .env file to /opt/datachain-rope/code/deploy/        ║"
echo "║  4. Run: cd /opt/datachain-rope/code/deploy                   ║"
echo "║  5. Run: docker-compose up -d                                  ║"
echo "╚════════════════════════════════════════════════════════════════╝"

