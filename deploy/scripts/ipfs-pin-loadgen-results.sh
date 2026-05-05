#!/usr/bin/env bash
# IPFS Pin Loadgen Results — content-addressed archive of every
# rope-loadgen benchmark run on this host.
#
# What it pins:
#   1. Each ~/loadgen-results-YYYY-MM-DD/ directory (recursive, wrapped)
#   2. The aggregated manifest mapping date → CID over time
#   3. Cross-pins each new CID on the secondary VPS so a single host loss
#      does not orphan the benchmark history
#
# What it produces:
#   - /opt/datachain-rope/ipfs-data/loadgen-manifest.json      ← cumulative
#   - /var/log/ipfs-loadgen.log                                ← run log
#   - /home/ubuntu/loadgen-cids.txt                            ← human index
#
# Schedule (installed by the matching cron entry):
#   */30 * * * *  /opt/datachain-rope/scripts/ipfs-pin-loadgen-results.sh \
#                  >> /var/log/ipfs-loadgen.log 2>&1
#
# Idempotent. Re-running a pin against unchanged content returns the same
# CID and is essentially free (IPFS deduplicates by content hash).

set -uo pipefail

export IPFS_PATH="${IPFS_PATH:-/opt/datachain-rope/ipfs}"

LOADGEN_GLOB="/home/ubuntu/loadgen-results-*"
IPFS_DATA_DIR="/opt/datachain-rope/ipfs-data"
MANIFEST="$IPFS_DATA_DIR/loadgen-manifest.json"
HUMAN_INDEX="/home/ubuntu/loadgen-cids.txt"
# Cross-pin to the secondary VPS via SSH because the IPFS HTTP API is
# (correctly) only bound to 127.0.0.1 on both nodes. The ed25519 key on
# rope-vps was authorized on anvil-vps as part of the cross-VPS sync setup
# (see ~/.ssh/id_ed25519 + the existing cross-pin pattern).
SECONDARY_HOST="ubuntu@92.243.25.119"
SECONDARY_SSH_KEY="${SECONDARY_SSH_KEY:-/home/ubuntu/.ssh/id_ed25519}"

TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
TAG=$(date -u +%Y%m%d-%H%M)
LOG_PREFIX="[ipfs-loadgen $TAG]"
log() { echo "$LOG_PREFIX $*"; }

mkdir -p "$IPFS_DATA_DIR"
[ -f "$MANIFEST" ] || echo '{"runs":[]}' > "$MANIFEST"

# Quick health check
if ! ipfs id >/dev/null 2>&1; then
    log "FATAL: local IPFS daemon not reachable (IPFS_PATH=$IPFS_PATH)"
    exit 1
fi

shopt -s nullglob
DIRS=( $LOADGEN_GLOB )
shopt -u nullglob

if [ ${#DIRS[@]} -eq 0 ]; then
    log "no loadgen-results-* directories found, nothing to pin"
    exit 0
fi

NEW_PINS=0
for DIR in "${DIRS[@]}"; do
    [ -d "$DIR" ] || continue
    NAME=$(basename "$DIR")
    SIZE=$(du -sb "$DIR" | awk '{print $1}')
    FILES=$(find "$DIR" -type f | wc -l)

    # Add (idempotent: same content → same CID; re-add is cheap)
    ROOT_CID=$(ipfs add -rqQ "$DIR" 2>/dev/null) || {
        log "WARN: ipfs add failed for $DIR"
        continue
    }

    # Skip if this exact (name, root_cid) tuple already in manifest
    ALREADY=$(python3 - "$MANIFEST" "$NAME" "$ROOT_CID" <<'PY'
import json, sys
manifest = json.load(open(sys.argv[1]))
name, cid = sys.argv[2], sys.argv[3]
print(any(r["name"] == name and r["root_cid"] == cid for r in manifest["runs"]))
PY
)
    if [ "$ALREADY" = "True" ]; then
        log "  $NAME → $ROOT_CID (already in manifest, skipping)"
        continue
    fi

    # Per-file CIDs for direct fetch — use `ipfs ls` against the directory CID
    # because parsing `ipfs add -r` verbose output is fragile under progress
    # bars. `ipfs ls` returns clean tab-separated <cid> <size> <name> rows.
    PER_FILE_JSON=$(ipfs ls "$ROOT_CID" 2>/dev/null \
        | python3 -c "
import json, sys
items = []
for line in sys.stdin:
    parts = line.strip().split(maxsplit=2)
    if len(parts) >= 3:
        items.append({'cid': parts[0], 'size': int(parts[1]), 'path': parts[2]})
print(json.dumps(items))
" 2>/dev/null || echo "[]")

    log "  pinned $NAME → $ROOT_CID (size=$SIZE bytes, $FILES files)"

    # Cross-pin to secondary VPS via SSH (HTTP API is localhost-only on both).
    # Best-effort, non-fatal — next run retries any failures.
    SECONDARY_OK=false
    if [ -r "$SECONDARY_SSH_KEY" ] && \
       ssh -o ConnectTimeout=10 -o BatchMode=yes -i "$SECONDARY_SSH_KEY" \
           "$SECONDARY_HOST" \
           "IPFS_PATH=/opt/datachain-rope/ipfs ipfs pin add $ROOT_CID" \
           >/dev/null 2>&1; then
        log "    + cross-pinned to secondary (92.243.25.119)"
        SECONDARY_OK=true
    else
        log "    ! cross-pin to secondary failed (will retry next run)"
    fi

    # Append to manifest
    python3 - "$MANIFEST" "$NAME" "$ROOT_CID" "$SIZE" "$FILES" "$TS" "$SECONDARY_OK" "$PER_FILE_JSON" <<'PY'
import json, sys
manifest_path, name, cid, size, files, ts, sec_ok, per_file = sys.argv[1:9]
manifest = json.load(open(manifest_path))
manifest["runs"].append({
    "name": name,
    "root_cid": cid,
    "size_bytes": int(size),
    "file_count": int(files),
    "pinned_at": ts,
    "cross_pinned_secondary": sec_ok == "True" or sec_ok == "true",
    "files": json.loads(per_file) if per_file else [],
    "gateways": [
        f"https://ipfs.io/ipfs/{cid}",
        f"https://dweb.link/ipfs/{cid}",
        f"ipfs://{cid}",
    ],
})
manifest["updated_at"] = ts
manifest["count"] = len(manifest["runs"])
with open(manifest_path, "w") as f:
    json.dump(manifest, f, indent=2)
PY

    NEW_PINS=$((NEW_PINS + 1))
done

# Pin the manifest itself so it can be referenced via a stable IPNS-able CID
MANIFEST_CID=$(ipfs add -qQ "$MANIFEST" 2>/dev/null) || true
if [ -n "$MANIFEST_CID" ]; then
    log "manifest pinned at $MANIFEST_CID"
fi

# Refresh the human index
{
    echo "# Datachain Rope — loadgen-results IPFS index"
    echo "# Last updated: $TS"
    echo "# Manifest CID: $MANIFEST_CID"
    echo
    python3 - "$MANIFEST" <<'PY'
import json, sys
m = json.load(open(sys.argv[1]))
for r in sorted(m["runs"], key=lambda r: r["name"], reverse=True):
    sec = "+secondary" if r.get("cross_pinned_secondary") else ""
    print(f"{r['name']:35s} {r['root_cid']}  ({r['size_bytes']} B, {r['file_count']} files) {sec}")
PY
} > "$HUMAN_INDEX"

log "done — $NEW_PINS new pin(s); ${#DIRS[@]} dir(s) total in manifest"
