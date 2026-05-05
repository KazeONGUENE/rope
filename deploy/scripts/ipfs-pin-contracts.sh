#!/usr/bin/env bash
# IPFS Pin Contract Deployments — Pins bytecode, ABI, and deployment metadata
#
# For every known contract on Datachain Rope, this script:
#   1. Fetches the deployed bytecode via eth_getCode
#   2. Creates a deployment receipt JSON with address, bytecode hash, block context
#   3. Pins everything to IPFS
#   4. Maintains a contracts manifest for cross-referencing
#
# Run: on-demand after deployments, or weekly via cron
# Cron: 0 3 * * 1 /opt/datachain-rope/scripts/ipfs-pin-contracts.sh >> /var/log/ipfs-contracts.log 2>&1

set -uo pipefail

export IPFS_PATH=/opt/datachain-rope/ipfs

CHAIN_RPC="http://127.0.0.1:8595"
IPFS_DATA="/opt/datachain-rope/ipfs-data"
MANIFEST="$IPFS_DATA/contracts-manifest.json"
TMP_DIR="/tmp/ipfs-contracts"
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)
LOG_PREFIX="[ipfs-contracts]"

log() { echo "$LOG_PREFIX $1"; }
mkdir -p "$IPFS_DATA" "$TMP_DIR"

BLOCK_HEX=$(curl -sf -X POST -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
    "$CHAIN_RPC" | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])")

log "=== Pinning contracts at block $BLOCK_HEX ==="

declare -A CONTRACTS=(
    ["WFAT"]="0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4"
    ["USDC"]="0xb93bd8db94f1baff474aa9cba0739daaad01641f"
    ["USDT"]="0x79a26132f48394421382c13b54ae77fa3af73289"
    ["EUROD"]="0x24d6137807fa8a592888726d87ac748d018c6d4a"
    ["DCSwapFactory"]="0x772e5fd559069aecce5e6983c0c415c8579d780d"
    ["DCSwapRouter"]="0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4"
    ["Multicall3"]="0xc2eeb0100aa7e81a3193bdce6733ff767f3bb93a"
    ["FAT_USDC_Pool"]="0xd9ebc3da001618a3ae90481d33ae7ef85e130317"
    ["FAT_USDT_Pool"]="0x644da44bcd5f453c593781dbe22dfd733e8d1441"
    ["FAT_EUROD_Pool"]="0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4"
    ["USDC_USDT_Pool"]="0xb86bdcecad93573d6ca21313aa7eac52800513c8"
    ["IdentityImplementation"]="0xe158A7b8030Af5386AAE3baE4fc7382200064f20"
    ["ImplementationAuthority"]="0x285EECF51D5f0a6Ab8D8151139b4D19B05c6b3E4"
    ["IdFactory"]="0xB93Bd8Db94F1bAfF474AA9cbA0739daaad01641F"
    ["ClaimTopicsRegistry"]="0x79a26132f48394421382C13B54Ae77fa3aF73289"
    ["TrustedIssuersRegistry"]="0x094237118686feF3b03Af028721C2e5C23027455"
    ["IdentityRegistryStorage"]="0xE3D48836733C4eBAF504694aa5D15d6f8F22FbF2"
    ["IdentityRegistry"]="0xB28E38b344A7238C9777D74209F966D1873D26e0"
    ["DatawalletClaimIssuer"]="0x34Ab12Ca0bc2cFb3510cCa479Cc5Bd4Eb6EAE883"
    ["RopeComplianceModule"]="0x30Ed28E33Fcd73705bDdA7c4246CF51F3d544cA6"
    ["DeployerONCHAINID"]="0x6A74c57C3A1EE72D9d2cA29462fbD6fc8fE86bd2"
    ["TanastokONCHAINID"]="0xE9D4fd64DF93fe848fE13303EAa28008feb72789"
)

RESULTS="[]"
PINNED=0

for NAME in "${!CONTRACTS[@]}"; do
    ADDR="${CONTRACTS[$NAME]}"
    ADDR_LOWER=$(echo "$ADDR" | tr '[:upper:]' '[:lower:]')

    CODE=$(curl -sf -X POST -H "Content-Type: application/json" \
        -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_getCode\",\"params\":[\"$ADDR\",\"latest\"],\"id\":1}" \
        "$CHAIN_RPC" | python3 -c "import json,sys; print(json.load(sys.stdin)['result'])" 2>/dev/null)

    if [ -z "$CODE" ] || [ "$CODE" = "0x" ]; then
        log "  SKIP $NAME ($ADDR) — no code deployed"
        continue
    fi

    CODE_SIZE=$(( ${#CODE} / 2 - 1 ))

    RECEIPT_FILE="$TMP_DIR/${NAME}.json"
    python3 -c "
import json, hashlib
code = '$CODE'
code_hash = hashlib.sha256(bytes.fromhex(code[2:])).hexdigest() if len(code) > 2 else 'empty'
receipt = {
    'schema': 'datachain-rope-contract-receipt-v1',
    'name': '$NAME',
    'address': '$ADDR_LOWER',
    'chainId': 271828,
    'snapshotBlock': '$BLOCK_HEX',
    'codeSize': $CODE_SIZE,
    'codeSha256': code_hash,
    'timestamp': '$TIMESTAMP'
}
with open('$RECEIPT_FILE', 'w') as f:
    json.dump(receipt, f, indent=2)
"

    BYTECODE_FILE="$TMP_DIR/${NAME}.bin"
    echo "$CODE" > "$BYTECODE_FILE"

    DIR_CID=$(ipfs add -Q --pin=true -r "$TMP_DIR/${NAME}.json" "$TMP_DIR/${NAME}.bin" 2>/dev/null)

    RECEIPT_CID=$(ipfs add -Q --pin=true "$RECEIPT_FILE")
    CODE_CID=$(ipfs add -Q --pin=true "$BYTECODE_FILE")

    log "  $NAME: receipt=$RECEIPT_CID code=$CODE_CID (${CODE_SIZE} bytes)"
    PINNED=$((PINNED + 1))

    RESULTS=$(echo "$RESULTS" | python3 -c "
import json,sys
arr = json.load(sys.stdin)
arr.append({
    'name': '$NAME',
    'address': '$ADDR_LOWER',
    'receiptCid': '$RECEIPT_CID',
    'bytecodeCid': '$CODE_CID',
    'codeSize': $CODE_SIZE
})
print(json.dumps(arr))
")

    rm -f "$RECEIPT_FILE" "$BYTECODE_FILE"
done

python3 -c "
import json
manifest = {
    'schema': 'datachain-rope-contracts-manifest-v1',
    'chainId': 271828,
    'timestamp': '$TIMESTAMP',
    'snapshotBlock': '$BLOCK_HEX',
    'totalContracts': $PINNED,
    'contracts': $RESULTS
}
with open('$MANIFEST', 'w') as f:
    json.dump(manifest, f, indent=2)
"

MANIFEST_CID=$(ipfs add -Q --pin=true "$MANIFEST")

log "=== Contract Pinning Complete ==="
log "  Contracts pinned: $PINNED"
log "  Manifest CID: $MANIFEST_CID"
