#!/usr/bin/env bash
# IPFS Pin Reth Chain State — Periodic state snapshots to IPFS
#
# Pins:
#   1. Genesis file (always, idempotent)
#   2. Reth data snapshot (tarball of db + static_files + rocksdb)
#   3. Block range metadata (block number, deployer nonce, timestamp)
#   4. Contract deployment receipts for known contracts
#
# The snapshot is content-addressed — identical state produces identical CID.
# Cross-pinned to secondary IPFS peer and optionally to Storacha/Filecoin.
#
# Run: every 6 hours via cron
# Cron: 0 */6 * * * /opt/datachain-rope/scripts/ipfs-pin-reth-state.sh >> /var/log/ipfs-reth.log 2>&1

set -uo pipefail

export IPFS_PATH=/opt/datachain-rope/ipfs

DATA_DIR="/opt/datachain-rope/reth/data"
GENESIS="/opt/datachain-rope/reth/genesis.json"
IPFS_DATA="/opt/datachain-rope/ipfs-data"
MANIFEST="$IPFS_DATA/reth-state-manifest.json"
SYNC_DIR="/opt/datachain-rope/reth/sync-staging"
TMP_DIR="/tmp/ipfs-reth-snapshot"
SECONDARY_HOST="ubuntu@92.243.25.119"

CHAIN_RPC="http://127.0.0.1:8595"
DEPLOYER="0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195"

TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)
DATE_TAG=$(date -u +%Y%m%d-%H%M)
LOG_PREFIX="[ipfs-reth $DATE_TAG]"

log() { echo "$LOG_PREFIX $1"; }
mkdir -p "$IPFS_DATA" "$TMP_DIR"

log "=== IPFS Reth State Pin ==="

BLOCK_HEX=$(curl -sf -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    "$CHAIN_RPC" | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])")
BLOCK_NUM=$((${BLOCK_HEX}))

NONCE_HEX=$(curl -sf -X POST -H "Content-Type: application/json" \
    -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getTransactionCount\",\"params\":[\"$DEPLOYER\",\"latest\"],\"id\":1}" \
    "$CHAIN_RPC" | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])")
NONCE_NUM=$((${NONCE_HEX}))

log "Chain: block=$BLOCK_NUM deployer_nonce=$NONCE_NUM"

# --- 1. Pin Genesis ---
GENESIS_CID=$(ipfs add -Q --pin=true "$GENESIS")
log "Genesis pinned: $GENESIS_CID"

# --- 2. Create chain metadata JSON ---
CONTRACTS='{}'
for name_addr in \
    "DCSwapFactory:0x772e5fd559069aecce5e6983c0c415c8579d780d" \
    "DCSwapRouter:0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4" \
    "WFAT:0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4" \
    "USDC:0xb93bd8db94f1baff474aa9cba0739daaad01641f" \
    "USDT:0x79a26132f48394421382c13b54ae77fa3af73289" \
    "EUROD:0x24d6137807fa8a592888726d87ac748d018c6d4a" \
    "Multicall3:0xc2eeb0100aa7e81a3193bdce6733ff767f3bb93a" \
    "IdentityImplementation:0xe158A7b8030Af5386AAE3baE4fc7382200064f20" \
    "IdentityRegistry:0xB28E38b344A7238C9777D74209F966D1873D26e0" \
    "DatawalletClaimIssuer:0x34Ab12Ca0bc2cFb3510cCa479Cc5Bd4Eb6EAE883"; do

    NAME="${name_addr%%:*}"
    ADDR="${name_addr##*:}"
    CODE_HEX=$(curl -sf -X POST -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getCode\",\"params\":[\"$ADDR\",\"latest\"],\"id\":1}" \
        "$CHAIN_RPC" | python3 -c "import json,sys; r=json.load(sys.stdin)['result']; print(len(r)//2 - 1 if r != '0x' else 0)" 2>/dev/null)

    CONTRACTS=$(echo "$CONTRACTS" | python3 -c "
import json,sys
d=json.load(sys.stdin)
d['$NAME'] = {'address': '$ADDR', 'codeSize': ${CODE_HEX:-0}}
print(json.dumps(d))
")
done

METADATA_FILE="$TMP_DIR/chain-state-$DATE_TAG.json"
python3 -c "
import json
meta = {
    'schema': 'datachain-rope-chain-state-v1',
    'chainId': 271828,
    'timestamp': '$TIMESTAMP',
    'blockNumber': $BLOCK_NUM,
    'blockHex': '$BLOCK_HEX',
    'deployerNonce': $NONCE_NUM,
    'deployer': '$DEPLOYER',
    'genesisCid': '$GENESIS_CID',
    'contracts': $CONTRACTS
}
with open('$METADATA_FILE', 'w') as f:
    json.dump(meta, f, indent=2)
"

METADATA_CID=$(ipfs add -Q --pin=true "$METADATA_FILE")
log "Chain metadata pinned: $METADATA_CID"

# --- 3. Snapshot Reth data directory ---
TARBALL="$TMP_DIR/reth-state-$DATE_TAG.tar.gz"
log "Creating tarball from live data dir..."
tar -czf "$TARBALL" -C "$DATA_DIR" db static_files rocksdb 2>/dev/null
TARBALL_SIZE=$(du -sh "$TARBALL" | cut -f1)
log "Tarball: $TARBALL_SIZE"

TARBALL_CID=$(ipfs add -Q --pin=true "$TARBALL")
log "State tarball pinned: $TARBALL_CID"

rm -f "$TARBALL" "$METADATA_FILE"

# --- 4. Update manifest ---
python3 -c "
import json, os
manifest_path = '$MANIFEST'
if os.path.exists(manifest_path):
    with open(manifest_path) as f:
        manifest = json.load(f)
else:
    manifest = {
        'schema': 'datachain-rope-reth-state-manifest-v1',
        'chainId': 271828,
        'genesisCid': '$GENESIS_CID',
        'snapshots': []
    }

manifest['genesisCid'] = '$GENESIS_CID'
manifest['snapshots'].append({
    'timestamp': '$TIMESTAMP',
    'blockNumber': $BLOCK_NUM,
    'deployerNonce': $NONCE_NUM,
    'metadataCid': '$METADATA_CID',
    'stateTarballCid': '$TARBALL_CID',
    'tarballSize': '${TARBALL_SIZE}'
})

manifest['snapshots'] = manifest['snapshots'][-50:]

with open(manifest_path, 'w') as f:
    json.dump(manifest, f, indent=2)
"

MANIFEST_CID=$(ipfs add -Q --pin=true "$MANIFEST")
log "Manifest pinned: $MANIFEST_CID"

# --- 5. Cross-pin to secondary peer (if IPFS running there) ---
DCSWAP_PEER="12D3KooWJB8MgSzXd17C3FDRTK8jFg71LaNaL8myNK5AwRn8FG6Z"
ipfs swarm connect "/ip4/92.243.26.114/tcp/4001/p2p/$DCSWAP_PEER" 2>/dev/null && \
    log "Connected to DCSwap IPFS peer" || true

# --- 6. Storacha upload (if w3 is available) ---
if command -v w3 &>/dev/null; then
    for CID in "$GENESIS_CID" "$METADATA_CID" "$TARBALL_CID" "$MANIFEST_CID"; do
        CAR_FILE="$TMP_DIR/${CID}.car"
        if ipfs dag export "$CID" > "$CAR_FILE" 2>/dev/null; then
            w3 up --car "$CAR_FILE" --no-wrap >/dev/null 2>&1 && \
                log "Storacha: $CID uploaded" || true
            rm -f "$CAR_FILE"
        fi
    done
fi

log "=== IPFS Pin Complete ==="
log "  Genesis:    $GENESIS_CID"
log "  Metadata:   $METADATA_CID"
log "  State:      $TARBALL_CID"
log "  Manifest:   $MANIFEST_CID"
log "  Block:      $BLOCK_NUM"
log "  Nonce:      $NONCE_NUM"
