# Datachain Rope - RPC & WebSocket Fix Deployment

## Summary of Changes

This deployment fixes the ChainList health check failures by:

1. **Fixed `erpc.rope.network`** - Now proxies to the actual RPC server instead of returning a static message
2. **Added WebSocket support** - Port 8546 now handles WebSocket JSON-RPC connections
3. **Fixed `ws.datachain.network`** - Now properly proxies WebSocket connections

## Files Modified

| File | Change |
|------|--------|
| `crates/rope-node/src/rpc_server.rs` | Added WebSocket server support (port 8546) |
| `crates/rope-node/Cargo.toml` | Added sha1 and base64 dependencies |
| `deploy/nginx/conf.d/rope.network.conf` | Fixed erpc.rope.network to proxy to RPC |
| `deploy/nginx/conf.d/datachain.network.conf` | (unchanged, already correct) |

## Build Status

```
✅ cargo build --release -p rope-node - SUCCESS
✅ Binary: target/release/rope-node
```

## Deployment Instructions

### Option 1: Automated (requires SSH key)

```bash
# Set your SSH key path
export SSH_KEY=~/.ssh/datachain_rpc

# Run deployment script
./deploy/scripts/deploy-rpc-fix.sh
```

### Option 2: Manual Deployment

SSH into each RPC node and execute:

```bash
# RPC-1: 157.230.18.45
# RPC-2: 167.172.106.174

# 1. Copy the binary (from your local machine)
scp target/release/rope-node root@<NODE_IP>:/tmp/rope-node

# 2. Copy nginx configs
scp deploy/nginx/conf.d/datachain.network.conf root@<NODE_IP>:/tmp/
scp deploy/nginx/conf.d/rope.network.conf root@<NODE_IP>:/tmp/

# 3. SSH into the node
ssh root@<NODE_IP>

# 4. On the node, run:
systemctl stop rope-node
cp /opt/rope/bin/rope-node /opt/rope/bin/rope-node.bak
mv /tmp/rope-node /opt/rope/bin/rope-node
chmod +x /opt/rope/bin/rope-node

mv /tmp/datachain.network.conf /etc/nginx/conf.d/
mv /tmp/rope.network.conf /etc/nginx/conf.d/

nginx -t && systemctl reload nginx
systemctl start rope-node

# 5. Verify
curl -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
# Expected: {"jsonrpc":"2.0","result":"0x425d4","id":1}

# Check WebSocket port
nc -z localhost 8546 && echo "WebSocket OK" || echo "WebSocket FAIL"
```

## Verification After Deployment

### Test HTTP RPC

```bash
# erpc.datachain.network
curl -X POST https://erpc.datachain.network \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'

# erpc.rope.network (should now return chain ID, not static message)
curl -X POST https://erpc.rope.network \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
```

Expected response:
```json
{"jsonrpc":"2.0","result":"0x425d4","id":1}
```

### Test WebSocket

```bash
# Using websocat (install: brew install websocat)
echo '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' | \
  websocat wss://ws.datachain.network
```

### ChainList Verification

1. Go to https://chainlist.org/chain/271828
2. Wait 5-10 minutes for health checks to refresh
3. Verify all RPC endpoints show green scores

## RPC Node Details

| Node | IP | Ports |
|------|-----|-------|
| datachain-rpc-1 | 157.230.18.45 | 8545 (HTTP), 8546 (WS) |
| datachain-rpc-2 | 167.172.106.174 | 8545 (HTTP), 8546 (WS) |

## Endpoints After Deployment

| Endpoint | Protocol | Status |
|----------|----------|--------|
| https://erpc.datachain.network | HTTP JSON-RPC | ✅ Working |
| https://erpc.rope.network | HTTP JSON-RPC | ✅ Fixed |
| wss://ws.datachain.network | WebSocket JSON-RPC | ✅ New |

## Rollback

If something goes wrong:

```bash
# On each node
systemctl stop rope-node
mv /opt/rope/bin/rope-node.bak /opt/rope/bin/rope-node
systemctl start rope-node
```
