#!/usr/bin/env bash
# IPFS Cross-Pin & Storacha Backup — Datachain Rope
#
# 1. Re-pins all recursive CIDs locally (ensures nothing is GC'd)
# 2. Cross-pins to DCSwap IPFS peer via Bitswap
# 3. Uploads new CIDs to Storacha (Filecoin) for long-term persistence
# 4. Cross-pins to secondary VPS IPFS peer (when available)
#
# Storacha account: contact@datachain.one
# Space: did:key:z6Mkt9te34Y6ciiJ5NoAJTaUkzDapcxwHbCxKsQoHU1iHebG
#
# Run: weekly
# Cron: 0 4 * * 1 /opt/datachain-rope/scripts/ipfs-crosspin-storacha.sh >> /var/log/ipfs-crosspin.log 2>&1

set -uo pipefail

export IPFS_PATH=/opt/datachain-rope/ipfs

IPFS_DATA="/opt/datachain-rope/ipfs-data"
CROSSPIN_LOG="$IPFS_DATA/crosspin-log.json"
STORACHA_LOG="$IPFS_DATA/storacha-upload-log.json"
TMP_DIR="/tmp/ipfs-crosspin"
TIMESTAMP=$(date -u +%Y-%m-%dT%H:%M:%SZ)
LOG_PREFIX="[crosspin]"

log() { echo "$LOG_PREFIX $1"; }
mkdir -p "$IPFS_DATA" "$TMP_DIR"

log "=== Cross-Pin & Storacha Upload ==="

CRITICAL_CIDS=()
CRITICAL_LABELS=()

collect_from_manifest() {
    local file="$1"
    local label="$2"
    if [ -f "$file" ]; then
        local cids
        cids=$(python3 -c "
import json
with open('$file') as f: data = json.load(f)
cids = set()
def extract(obj):
    if isinstance(obj, dict):
        for k, v in obj.items():
            if k in ('cid','receiptCid','bytecodeCid','metadataCid','stateTarballCid','genesisCid','manifestCid') and isinstance(v, str) and (v.startswith('Qm') or v.startswith('bafy')):
                cids.add(v)
            else:
                extract(v)
    elif isinstance(obj, list):
        for item in obj:
            extract(item)
extract(data)
for c in cids:
    print(c)
" 2>/dev/null)
        while IFS= read -r cid; do
            if [ -n "$cid" ]; then
                CRITICAL_CIDS+=("$cid")
                CRITICAL_LABELS+=("$label")
            fi
        done <<< "$cids"
    fi
}

collect_from_manifest "$IPFS_DATA/reth-state-manifest.json" "reth-state"
collect_from_manifest "$IPFS_DATA/contracts-manifest.json" "contracts"

ALL_PINS=$(ipfs pin ls --type=recursive -q 2>/dev/null)
while IFS= read -r pin; do
    found=false
    for c in "${CRITICAL_CIDS[@]:-}"; do
        if [ "$c" = "$pin" ]; then found=true; break; fi
    done
    if ! $found && [ -n "$pin" ]; then
        CRITICAL_CIDS+=("$pin")
        CRITICAL_LABELS+=("existing-pin")
    fi
done <<< "$ALL_PINS"

log "Found ${#CRITICAL_CIDS[@]} CIDs to cross-pin"

PINNED=0
FAILED=0
for i in "${!CRITICAL_CIDS[@]}"; do
    CID="${CRITICAL_CIDS[$i]}"
    LABEL="${CRITICAL_LABELS[$i]}"
    ipfs pin add "$CID" 2>/dev/null && {
        PINNED=$((PINNED + 1))
    } || {
        FAILED=$((FAILED + 1))
        log "  FAIL: $CID ($LABEL)"
    }
done
log "Local re-pin: $PINNED OK, $FAILED failed"

# --- DCSwap IPFS peer ---
DCSWAP_PEER="12D3KooWJB8MgSzXd17C3FDRTK8jFg71LaNaL8myNK5AwRn8FG6Z"
ipfs swarm connect "/ip4/92.243.26.114/tcp/4001/p2p/$DCSWAP_PEER" 2>/dev/null && \
    log "Connected to DCSwap IPFS peer — CIDs propagate via Bitswap" || \
    log "DCSwap IPFS peer not reachable"

# --- Secondary VPS IPFS peer (if running) ---
SECONDARY_PEER_ID=$(ssh -o ConnectTimeout=3 -o BatchMode=yes ubuntu@92.243.25.119 \
    'IPFS_PATH=/opt/datachain-rope/ipfs ipfs id -f "<id>" 2>/dev/null' 2>/dev/null || echo "")
if [ -n "$SECONDARY_PEER_ID" ]; then
    ipfs swarm connect "/ip4/92.243.25.119/tcp/4001/p2p/$SECONDARY_PEER_ID" 2>/dev/null && \
        log "Connected to secondary IPFS peer ($SECONDARY_PEER_ID)" || \
        log "Secondary IPFS peer connection failed"
else
    log "Secondary IPFS not running (will be set up by ipfs-setup-secondary.sh)"
fi

# --- Storacha upload ---
STORACHA_NEW=0
STORACHA_SKIP=0

if command -v w3 &>/dev/null; then
    ALREADY_UPLOADED=""
    if [ -f "$STORACHA_LOG" ]; then
        ALREADY_UPLOADED=$(python3 -c "
import json
with open('$STORACHA_LOG') as f: data = json.load(f)
for c in data.get('uploads', []):
    print(c)
" 2>/dev/null)
    fi

    for i in "${!CRITICAL_CIDS[@]}"; do
        CID="${CRITICAL_CIDS[$i]}"
        if echo "$ALREADY_UPLOADED" | grep -qF "$CID"; then
            STORACHA_SKIP=$((STORACHA_SKIP + 1))
            continue
        fi

        CAR_FILE="$TMP_DIR/${CID}.car"
        if ipfs dag export "$CID" > "$CAR_FILE" 2>/dev/null; then
            CAR_SIZE=$(stat -c%s "$CAR_FILE" 2>/dev/null || stat -f%z "$CAR_FILE" 2>/dev/null || echo "0")
            if [ "${CAR_SIZE:-0}" -gt 10 ] 2>/dev/null; then
                if w3 up --car "$CAR_FILE" --no-wrap >/dev/null 2>&1; then
                    log "  Storacha OK: $CID (${CAR_SIZE} bytes)"
                    STORACHA_NEW=$((STORACHA_NEW + 1))
                fi
            fi
        fi
        rm -f "$CAR_FILE"
    done

    log "Storacha: $STORACHA_NEW new, $STORACHA_SKIP skipped"

    if command -v w3 &>/dev/null; then
        w3 ls 2>/dev/null > /tmp/w3-ls-output.txt
        python3 -c "
import json, datetime
with open('/tmp/w3-ls-output.txt') as f:
    cids = [l.strip() for l in f if l.strip().startswith('Qm') or l.strip().startswith('bafy')]
data = {
    'schema': 'datachain-rope-storacha-v1',
    'timestamp': datetime.datetime.utcnow().strftime('%Y-%m-%dT%H:%M:%SZ'),
    'space': 'did:key:z6Mkt9te34Y6ciiJ5NoAJTaUkzDapcxwHbCxKsQoHU1iHebG',
    'total_uploaded': len(cids),
    'uploads': cids
}
with open('$STORACHA_LOG', 'w') as f:
    json.dump(data, f, indent=2)
" 2>/dev/null
    fi
else
    log "Storacha CLI (w3) not installed — skipping Filecoin backup"
fi

python3 -c "
import json, os, datetime
path = '$CROSSPIN_LOG'
if os.path.exists(path):
    with open(path) as f: data = json.load(f)
else:
    data = {'project': 'Datachain Rope Cross-Pin', 'reports': []}
data['reports'].append({
    'timestamp': '$TIMESTAMP',
    'total': ${#CRITICAL_CIDS[@]},
    'pinned': $PINNED,
    'failed': $FAILED,
    'storacha_new': $STORACHA_NEW
})
data['reports'] = data['reports'][-100:]
with open(path, 'w') as f:
    json.dump(data, f, indent=2)
"

log "=== Cross-Pin Complete ==="
log "  Total CIDs: ${#CRITICAL_CIDS[@]}"
log "  Pinned: $PINNED"
log "  Storacha new: $STORACHA_NEW"
