#!/bin/bash
# Deploy DC Explorer to Production Server
# Server: 92.243.26.189

set -e

SSH_KEY="~/.ssh/DCRope_key"
SERVER="ubuntu@92.243.26.189"
REMOTE_PATH="/opt/datachain-rope/code"

echo "╔══════════════════════════════════════════════════════════════╗"
echo "║          DC EXPLORER DEPLOYMENT                              ║"
echo "║          dcscan.io API Backend                               ║"
echo "╚══════════════════════════════════════════════════════════════╝"

# Step 1: Sync codebase to server
echo ""
echo "Step 1: Syncing codebase to server..."
rsync -avz --delete \
    --exclude 'target' \
    --exclude '.git' \
    --exclude 'node_modules' \
    -e "ssh -i $SSH_KEY" \
    "$(dirname "$0")/../" \
    "$SERVER:$REMOTE_PATH/"

# Step 2: Deploy nginx config
echo ""
echo "Step 2: Deploying nginx configuration..."
ssh -i $SSH_KEY $SERVER "sudo cp $REMOTE_PATH/deploy/nginx/conf.d/*.conf /opt/datachain-rope/code/deploy/nginx/conf.d/"

# Step 3: Run database migrations
echo ""
echo "Step 3: Running database migrations..."
ssh -i $SSH_KEY $SERVER "docker exec -i rope-postgres psql -U dcscan -d dcscan < $REMOTE_PATH/deploy/init-db/02-federation-community.sql 2>/dev/null || echo 'Migration already applied or skipped'"

# Step 4: Build and start the explorer
echo ""
echo "Step 4: Building and starting DC Explorer..."
ssh -i $SSH_KEY $SERVER "cd $REMOTE_PATH && docker compose -f deploy/docker-compose.yml build dc-explorer"
ssh -i $SSH_KEY $SERVER "cd $REMOTE_PATH && docker compose -f deploy/docker-compose.yml up -d dc-explorer"

# Step 5: Reload nginx
echo ""
echo "Step 5: Reloading nginx..."
ssh -i $SSH_KEY $SERVER "docker exec rope-nginx nginx -s reload"

# Step 6: Verify deployment
echo ""
echo "Step 6: Verifying deployment..."
sleep 5
ssh -i $SSH_KEY $SERVER "docker ps | grep dc-explorer"
ssh -i $SSH_KEY $SERVER "curl -s http://localhost:3001/health | head -20"

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║          DEPLOYMENT COMPLETE                                 ║"
echo "╠══════════════════════════════════════════════════════════════╣"
echo "║  API: https://dcscan.io/api/v1/status                       ║"
echo "║  API: https://api.dcscan.io/api/v1/status                   ║"
echo "║  Health: https://dcscan.io/api/v1/health                    ║"
echo "╚══════════════════════════════════════════════════════════════╝"

