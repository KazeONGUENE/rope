#!/bin/bash
# =============================================================================
# Datachain Rope - Deployment Script
# Run this to deploy/update the production environment
# =============================================================================

set -e

DEPLOY_DIR="/opt/datachain-rope/code/deploy"
SSL_DIR="/opt/datachain-rope/ssl"

echo "╔════════════════════════════════════════════════════════════════╗"
echo "║       DATACHAIN ROPE - PRODUCTION DEPLOYMENT                   ║"
echo "╚════════════════════════════════════════════════════════════════╝"

cd $DEPLOY_DIR

# Check .env file
if [ ! -f .env ]; then
    echo "❌ ERROR: .env file not found!"
    echo "   Copy env.production.example to .env and configure it."
    exit 1
fi

# Check SSL certificates
check_ssl() {
    local domain=$1
    if [ ! -f "$SSL_DIR/$domain/fullchain.pem" ] || [ ! -f "$SSL_DIR/$domain/privkey.pem" ]; then
        echo "❌ ERROR: SSL certificates for $domain not found!"
        echo "   Expected: $SSL_DIR/$domain/fullchain.pem"
        echo "   Expected: $SSL_DIR/$domain/privkey.pem"
        return 1
    fi
    return 0
}

echo "🔐 Checking SSL certificates..."
check_ssl "datachain.network" || exit 1
check_ssl "rope.network" || exit 1
check_ssl "dcscan.io" || exit 1
echo "✅ All SSL certificates found."

# Pull latest code
echo "📥 Pulling latest code..."
cd /opt/datachain-rope/code
git pull origin main

# Build containers
echo "🔨 Building containers..."
cd $DEPLOY_DIR
docker-compose build

# Deploy
echo "🚀 Deploying..."
docker-compose down || true
docker-compose up -d

# Health check
echo "🏥 Running health checks..."
sleep 10

check_service() {
    local name=$1
    local container=$2
    if docker ps | grep -q $container; then
        echo "  ✅ $name is running"
    else
        echo "  ❌ $name is NOT running!"
    fi
}

check_service "Rope Node" "rope-node"
check_service "PostgreSQL" "rope-postgres"
check_service "Redis" "rope-redis"
check_service "Nginx" "rope-nginx"

echo ""
echo "╔════════════════════════════════════════════════════════════════╗"
echo "║  🎉 DEPLOYMENT COMPLETE!                                       ║"
echo "║                                                                ║"
echo "║  Services:                                                     ║"
echo "║  • RPC: https://erpc.datachain.network                        ║"
echo "║  • RPC: https://erpc.rope.network                             ║"
echo "║  • WS:  wss://ws.datachain.network                            ║"
echo "║  • Explorer: https://dcscan.io                                ║"
echo "║                                                                ║"
echo "║  Logs: docker-compose logs -f                                 ║"
echo "╚════════════════════════════════════════════════════════════════╝"

