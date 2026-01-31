# Datachain Rope Network Deployment Report
## For Claude AI Context Continuation

**Report Date:** January 24, 2026  
**Report Type:** VPS Deployment & Network Identification  
**Status:** ✅ Successfully Deployed and Operational

---

## Executive Summary

This report documents the successful deployment of the Datachain Rope Bootstrap Node to production VPS infrastructure, along with critical network identification information required for ongoing development and node operations.

---

## 1. VPS Deployment Details

### 1.1 Infrastructure

| Parameter | Value |
|-----------|-------|
| **VPS IP Address** | `92.243.26.189` |
| **Hostname** | `dcrope` |
| **Operating System** | Ubuntu 24.04 LTS (Linux 6.8.0-31-generic x86_64) |
| **RAM** | 7.7 GB |
| **Disk Space** | 484 GB (469 GB available) |
| **SSH Access** | `ssh -i ~/.ssh/DCRope_key ubuntu@92.243.26.189` |

### 1.2 Deployment Package Location

```
/opt/datachain-rope/
├── rope                 # Compiled binary (Linux x86_64 ELF)
├── testnet.json         # Network configuration
├── code/                # Source code backup
├── data/                # Runtime data
├── logs/                # Log files
├── nginx/               # Web server config
└── ssl/                 # SSL certificates

/var/lib/datachain-rope/
├── db/                  # RocksDB database
└── keys/
    ├── node.key         # Ed25519 + Dilithium3 private key (4096 bytes)
    ├── node.pub         # Public key (3208 bytes)
    └── node.id          # Node ID hex (64 chars)
```

### 1.3 Systemd Service

```ini
# /etc/systemd/system/datachain-rope.service
[Unit]
Description=Datachain Rope Bootstrap Node
After=network.target

[Service]
Type=simple
User=datachain
Group=datachain
ExecStart=/opt/datachain-rope/rope node --network testnet --mode validator --data-dir /var/lib/datachain-rope
Restart=always
RestartSec=10
LimitNOFILE=65535
MemoryLimit=4G

[Install]
WantedBy=multi-user.target
```

---

## 2. Network Identification Information

### 2.1 Chain Identifiers

| Parameter | Value | Description |
|-----------|-------|-------------|
| **Chain ID (Testnet)** | `271829` | `0x425D5` in hex |
| **Chain ID (Mainnet)** | `271828` | `0x425D4` in hex (Euler's number e) |
| **Network Name** | Datachain Rope | Layer 0 Infrastructure |
| **Consensus Type** | Testimony-based | Virtual voting with OES |

### 2.2 Bootstrap Node Identity

| Parameter | Value |
|-----------|-------|
| **Peer ID** | `12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM` |
| **Node ID** | `6fd19624df05cb790d17903575013344b7f6432aa6a27473da157c0904585c15` |
| **Genesis Hash** | `2ad8d15258ba8167...` (truncated) |
| **Genesis String ID** | `3def8ccc822b6731...` (truncated) |

### 2.3 Bootstrap Multiaddr

**Primary (TCP):**
```
/ip4/92.243.26.189/tcp/9000/p2p/12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM
```

**QUIC (UDP):**
```
/ip4/92.243.26.189/udp/9000/quic-v1/p2p/12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM
```

### 2.4 Network Endpoints

| Service | Address | Protocol | Access |
|---------|---------|----------|--------|
| P2P (libp2p) | `92.243.26.189:9000` | TCP/QUIC | Public |
| JSON-RPC | `127.0.0.1:9001` | HTTP | Local |
| HTTP RPC | `127.0.0.1:8545` | HTTP | Local |
| WebSocket | `127.0.0.1:8546` | WS | Local |
| Metrics | `127.0.0.1:9090` | HTTP | Local |
| gRPC | `127.0.0.1:9001` | HTTP/2 | Local |

---

## 3. Latest Improvements Deployed

### 3.1 String Production System (NEW)

**File:** `crates/rope-node/src/string_producer.rs`

The String Producer module was implemented and integrated, enabling:

- **Anchor String Production:** Automatic generation of anchor strings at configurable intervals (default: 4200ms / 4.2 seconds)
- **Lamport Clock Integration:** Proper temporal ordering using distributed logical clocks
- **RopeString Builder Pattern:** Correct use of the builder API for string construction
- **Hybrid Signatures:** Ed25519 + CRYSTALS-Dilithium3 post-quantum signatures on all anchors

```rust
// Key components
pub struct StringProducer {
    config: StringProducerConfig,
    node_id: NodeId,
    keypair: KeyPair,
    clock: Arc<RwLock<LamportClock>>,
    event_tx: mpsc::Sender<ProductionEvent>,
    genesis_string_id: StringId,
    last_anchor_id: Arc<RwLock<StringId>>,
    current_round: Arc<RwLock<u64>>,
}
```

### 3.2 Fixed Bootstrap Node Configuration

**Issue:** Hardcoded peer ID was malformed  
**Solution:** Implemented `peer-id` CLI command to generate correct libp2p Peer IDs

```bash
# Generate peer ID from keypair
./rope peer-id --key /path/to/node.key --ip 92.243.26.189 --port 9000
```

### 3.3 Enabled Consensus for Testnet

**File:** `crates/rope-node/src/config.rs`

Changed default testnet configuration:
```rust
// Before
config.consensus.enabled = false;

// After  
config.consensus.enabled = true;
```

### 3.4 CLI Mode Override

**File:** `crates/rope-cli/src/main.rs`

CLI `--mode` argument now correctly overrides config file:
```rust
node_config.node.mode = mode.parse()
    .map_err(|e: String| anyhow::anyhow!("Invalid node mode: {}", e))?;
```

### 3.5 JSON-RPC Implementation

**File:** `crates/rope-node/src/rpc_server.rs`

Implemented Ethereum-compatible JSON-RPC methods:

| Method | Description | Response Example |
|--------|-------------|------------------|
| `eth_chainId` | Returns chain ID | `"0x4cb30"` |
| `eth_blockNumber` | Current anchor round | `"0x11"` |
| `net_version` | Network version | `"314160"` |
| `rope_getNetworkInfo` | Network details | `{chainId, peerCount, ...}` |
| `eth_gasPrice` | Gas price | `"0x3b9aca00"` (1 Gwei) |

### 3.6 Genesis State Generation

**File:** `crates/rope-node/src/genesis.rs`

Enhanced genesis generation with proper tokenomics:

| Parameter | Value |
|-----------|-------|
| Genesis Supply | 10 Billion FAT |
| Asymptotic Max | 18 Billion FAT |
| Era 1 Emission | 500M FAT/year |
| Halving Interval | 4 years |
| Block Time | 4200ms (4.2 seconds) |
| Min Stake | 1 FAT |

---

## 4. Current Operational Status

### 4.1 Service Status

```
● datachain-rope.service - Datachain Rope Bootstrap Node
     Loaded: loaded (/etc/systemd/system/datachain-rope.service; enabled)
     Active: active (running)
   Main PID: 1507078 (rope)
     Memory: ~3 MB
        CPU: minimal
```

### 4.2 String Production Output

```
🔗 Anchor #1 produced: e07714cdbb109276 (0.03ms)
🔗 Anchor #2 produced: 04001f1add81cc45 (0.04ms)
🔗 Anchor #3 produced: 2a677d92cbfa73ec (0.12ms)
...
🔗 Anchor #17 produced: f48954887a875674 (0.03ms)
```

### 4.3 Known Warnings (Expected)

```
WARN rope_network::swarm: Failed to publish to /rope/anchors/1.0.0: InsufficientPeers
```

This warning is **expected** because:
- This is the only node currently running
- GossipSub requires peers to publish messages
- Warning will resolve when additional nodes join the network

---

## 5. Protocol Topics (GossipSub)

The node subscribes to these protocol topics:

| Topic | Purpose |
|-------|---------|
| `/rope/strings/1.0.0` | String propagation |
| `/rope/gossip/1.0.0` | General gossip |
| `/rope/testimonies/1.0.0` | Consensus testimonies |
| `/rope/anchors/1.0.0` | Anchor announcements |

---

## 6. Cryptographic Configuration

### 6.1 Key Algorithms

| Component | Algorithm | Security Level |
|-----------|-----------|----------------|
| Classical Signing | Ed25519 | 128-bit |
| Post-Quantum Signing | CRYSTALS-Dilithium3 | NIST Level 3 |
| Key Encapsulation | CRYSTALS-Kyber768 | NIST Level 3 |
| Hashing | BLAKE3 | 256-bit |

### 6.2 Hybrid Signature Format

All strings are signed with both Ed25519 and Dilithium3:
```rust
pub struct HybridSignature {
    pub ed25519: [u8; 64],      // 64 bytes
    pub dilithium: Vec<u8>,     // ~3293 bytes
}
```

---

## 7. Configuration Files

### 7.1 testnet.json

```json
{
  "node": {
    "name": "rope-testnet-node",
    "mode": "relay",
    "chain_id": 271829
  },
  "network": {
    "listen_addr": "0.0.0.0:9000",
    "bootstrap_nodes": [
      "/ip4/92.243.26.189/tcp/9000/p2p/12D3KooWBXNzc2E4Z9CLypkRXro5iSdbM5oTnTkmf8ncZAqjhAfM"
    ],
    "max_peers": 50,
    "enable_quic": true,
    "enable_nat": true
  },
  "consensus": {
    "enabled": true,
    "block_time_ms": 3000,
    "min_testimonies": 5,
    "ai_agents_enabled": true
  },
  "storage": {
    "db_path": "~/.rope/testnet/db",
    "enable_compression": true,
    "cache_size_mb": 512,
    "pruning": "archive"
  },
  "rpc": {
    "enabled": true,
    "http_addr": "127.0.0.1:8545",
    "grpc_addr": "127.0.0.1:9001",
    "ws_addr": "127.0.0.1:8546",
    "enable_tls": false,
    "cors_origins": ["*"],
    "rate_limit": 100
  },
  "metrics": {
    "enabled": true,
    "prometheus_addr": "127.0.0.1:9090"
  }
}
```

---

## 8. Management Commands Reference

### 8.1 Service Management

```bash
# SSH into VPS
ssh -i ~/.ssh/DCRope_key ubuntu@92.243.26.189

# Check service status
sudo systemctl status datachain-rope

# View live logs
sudo journalctl -u datachain-rope -f

# Restart service
sudo systemctl restart datachain-rope

# Stop service
sudo systemctl stop datachain-rope

# Start service
sudo systemctl start datachain-rope
```

### 8.2 RPC Testing

```bash
# Get chain ID
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}' \
  http://127.0.0.1:9001

# Get block number
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
  http://127.0.0.1:9001

# Get network info
curl -s -X POST -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","method":"rope_getNetworkInfo","params":[],"id":1}' \
  http://127.0.0.1:9001
```

### 8.3 Peer ID Generation

```bash
./rope peer-id --key /path/to/node.key --ip <PUBLIC_IP> --port 9000
```

---

## 9. Files Modified in This Deployment

| File | Changes |
|------|---------|
| `crates/rope-cli/src/main.rs` | Added `peer-id` command, mode override |
| `crates/rope-cli/Cargo.toml` | Added libp2p dependency |
| `crates/rope-node/src/config.rs` | Enabled consensus for testnet |
| `crates/rope-node/src/node.rs` | Integrated StringProducer |
| `crates/rope-node/src/string_producer.rs` | **NEW** - String production |
| `crates/rope-node/src/genesis.rs` | Enhanced tokenomics |
| `crates/rope-node/src/rpc_server.rs` | RPC methods implementation |
| `crates/rope-core/src/types.rs` | Fixed StringId/NodeId constructors |
| `crates/rope-core/src/string.rs` | Fixed builder pattern |
| `config/networks/testnet.json` | Updated bootstrap multiaddr |
| `deploy/package/*` | Deployment package |

---

## 10. Next Steps (Recommendations)

1. **Deploy Additional Validators** - Add 2-4 more validator nodes to enable actual consensus
2. **Enable External RPC** - Configure nginx reverse proxy for public RPC access
3. **Implement Testimony Collection** - Wire up testimony gossip for virtual voting
4. **Add Monitoring** - Set up Prometheus/Grafana for metrics visualization
5. **Configure TLS** - Enable HTTPS for RPC endpoints
6. **Implement State Sync** - Allow new nodes to sync from existing validators

---

## 11. Contact & Resources

- **VPS Provider:** Gandi Cloud
- **Repository:** `/Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/datachain-rope`
- **SSH Key:** `~/.ssh/DCRope_key`

---

*Report generated for Claude AI context continuation on January 24, 2026*
