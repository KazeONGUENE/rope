#!/usr/bin/env bash
# ipfs-publish-bootstrap.sh — publish versioned node-bootstrap artifacts
# to the Datachain IPFS mesh so freshly provisioned DO/Exoscale nodes can
# self-bootstrap from the nearest gateway with content-hash verification,
# with no SSH trust to a central box.
#
# What it publishes (pinned on the local Kubo):
#   1. the canonical rope binary        (/usr/local/bin/rope by default)
#   2. the governance master-nodes file (if present)
#   3. a manifest.json that names both, with CIDs + sha256 + size + target
#
# The manifest is ALSO written into the dcscan static tree so it is served
# at   https://dcscan.io/bootstrap/manifest.json   over plain HTTPS. The
# manifest is the only thing fetched over HTTPS; the artifacts themselves
# come from any IPFS gateway and are verified by sha256 before install,
# so a compromised gateway cannot serve a tampered binary.
#
# Cron (BLUE, hourly is plenty — content only changes on deploys):
#   17 * * * * /opt/datachain-rope/scripts/ipfs-publish-bootstrap.sh >> /var/log/ipfs-bootstrap-publish.log 2>&1
#
# Idempotent: re-publishing an unchanged binary produces the same CID and
# the manifest is only rewritten when content actually changed.

set -euo pipefail

BINARY="${ROPE_BINARY:-/usr/local/bin/rope}"
# Kubo repo — rope-vps runs the daemon as ubuntu with this repo path.
export IPFS_PATH="${IPFS_PATH:-/opt/datachain-rope/ipfs}"
MASTER_NODES="${ROPE_MASTER_NODES:-/home/ubuntu/.rope/master-nodes.toml}"
# dcscan static root (nginx-served). The manifest lands in bootstrap/.
STATIC_ROOT="${DCSCAN_STATIC_ROOT:-/opt/datachain-rope/code/deploy/nginx/html/dcscan}"
OUT_DIR="${STATIC_ROOT}/bootstrap"
STATE_DIR="${BOOTSTRAP_STATE_DIR:-/opt/datachain-rope/bootstrap-publish}"
IPFS_BIN="${IPFS_BIN:-ipfs}"

log() { echo "[$(date -u +%FT%TZ)] $*"; }

command -v "$IPFS_BIN" >/dev/null 2>&1 || { log "ERROR: ipfs (Kubo) not on PATH"; exit 1; }
[ -f "$BINARY" ] || { log "ERROR: rope binary not found at $BINARY"; exit 1; }

mkdir -p "$OUT_DIR" "$STATE_DIR"

# ---------------------------------------------------------------------------
# 1. Fingerprint the current binary; short-circuit if nothing changed.
# ---------------------------------------------------------------------------
BIN_SHA256=$(sha256sum "$BINARY" | awk '{print $1}')
BIN_SIZE=$(stat -c%s "$BINARY" 2>/dev/null || stat -f%z "$BINARY")
LAST_SHA_FILE="$STATE_DIR/last-published-sha256"

if [ -f "$LAST_SHA_FILE" ] && [ "$(cat "$LAST_SHA_FILE")" = "$BIN_SHA256" ] \
   && [ -f "$OUT_DIR/manifest.json" ]; then
    log "binary unchanged (sha256=$BIN_SHA256) — manifest already current, nothing to do"
    exit 0
fi

# ---------------------------------------------------------------------------
# 2. Pin the artifacts. `ipfs add` is content-addressed: unchanged bytes
#    yield the same CID, so re-adds are cheap and idempotent.
# ---------------------------------------------------------------------------
log "adding rope binary to IPFS ($BINARY, $BIN_SIZE bytes) ..."
BIN_CID=$("$IPFS_BIN" add -Q --pin=true --cid-version=1 "$BINARY")
log "rope binary pinned: $BIN_CID"

MASTER_CID=""
MASTER_SHA256=""
if [ -f "$MASTER_NODES" ]; then
    MASTER_SHA256=$(sha256sum "$MASTER_NODES" | awk '{print $1}')
    MASTER_CID=$("$IPFS_BIN" add -Q --pin=true --cid-version=1 "$MASTER_NODES")
    log "master-nodes.toml pinned: $MASTER_CID"
fi

# Target triple: glibc pinning matters (2026-06-12 lesson: a noble binary
# does not run on jammy). Recorded so ropectl can refuse a mismatched image.
GLIBC_VERSION=$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+' | head -1)
[ -z "$GLIBC_VERSION" ] && GLIBC_VERSION="unknown"
OS_RELEASE=$( (. /etc/os-release 2>/dev/null && printf '%s-%s' "${ID}" "${VERSION_ID}") || echo "unknown")
# Defensive: strip any stray whitespace/newlines so the JSON stays valid.
GLIBC_VERSION=$(printf '%s' "$GLIBC_VERSION" | tr -d '[:space:]')
OS_RELEASE=$(printf '%s' "$OS_RELEASE" | tr -d '[:space:]')
VERSION="$(date -u +%Y%m%d-%H%M%S)-${BIN_SHA256:0:12}"

# ---------------------------------------------------------------------------
# 3. Write the manifest (atomic), keep append-only version history.
# ---------------------------------------------------------------------------
HISTORY_FILE="$STATE_DIR/versions.jsonl"
GATEWAYS='["https://dcswap.net/ipfs/", "https://ipfs.io/ipfs/", "https://dweb.link/ipfs/", "https://cloudflare-ipfs.com/ipfs/"]'

MANIFEST=$(cat <<EOF
{
  "schema": "dcrope-bootstrap-manifest/v1",
  "version": "$VERSION",
  "published_at": "$(date -u +%FT%TZ)",
  "chain_id": 271828,
  "target": {
    "os": "$OS_RELEASE",
    "arch": "$(uname -m)",
    "glibc": "$GLIBC_VERSION"
  },
  "artifacts": {
    "rope_binary": {
      "cid": "$BIN_CID",
      "sha256": "$BIN_SHA256",
      "size_bytes": $BIN_SIZE,
      "install_path": "/usr/local/bin/rope"
    }$( [ -n "$MASTER_CID" ] && cat <<EOM
,
    "master_nodes": {
      "cid": "$MASTER_CID",
      "sha256": "$MASTER_SHA256",
      "install_path": "/root/.rope/master-nodes.toml"
    }
EOM
)
  },
  "gateways": $GATEWAYS
}
EOF
)

echo "$MANIFEST" > "$OUT_DIR/manifest.json.tmp"
mv "$OUT_DIR/manifest.json.tmp" "$OUT_DIR/manifest.json"
log "manifest written to $OUT_DIR/manifest.json (version $VERSION)"

# Pin the manifest itself (cold archive of every published version).
MANIFEST_CID=$("$IPFS_BIN" add -Q --pin=true --cid-version=1 "$OUT_DIR/manifest.json")
log "manifest pinned: $MANIFEST_CID"

# Append to the version history (audit trail of everything ever published).
printf '{"version":"%s","published_at":"%s","binary_cid":"%s","binary_sha256":"%s","manifest_cid":"%s"}\n' \
    "$VERSION" "$(date -u +%FT%TZ)" "$BIN_CID" "$BIN_SHA256" "$MANIFEST_CID" >> "$HISTORY_FILE"
cp "$HISTORY_FILE" "$OUT_DIR/versions.jsonl" 2>/dev/null || true

echo "$BIN_SHA256" > "$LAST_SHA_FILE"
log "publish complete: version=$VERSION binary_cid=$BIN_CID manifest_cid=$MANIFEST_CID"
