#!/usr/bin/env bash
# Register this node on the Global Databox Network
# (https://dcscan.io/databoxes — see crates/rope-explorer/src/databox_registry.rs).
#
# Signature scheme: EIP-191 personal_sign, domain-tagged "DCROPE-DATABOX-AUTH"
# so this signature can never be replayed against any other Datachain Rope
# signing surface (votes, EDC, identity, ...). Any standard EVM wallet works.
#
# Requires: foundry's `cast` (https://getfoundry.sh) for signing, and curl.
#
# Usage:
#   ./register-databox.sh \
#     --private-key 0x... \
#     --name "my-node-01" \
#     --type databox \
#     --region eu-west \
#     [--city Paris] [--country FR] [--lat 48.85] [--lon 2.35] \
#     [--endpoint-url https://my-node.example.com] \
#     [--capacity-gb 500] \
#     [--registry-url https://dcscan.io]
#
# On success, writes DATABOX_ID + DATABOX_OWNER to <data-dir>/databox.env so
# the heartbeat timer/service can pick them up without re-signing.

set -euo pipefail

REGISTRY_URL="https://dcscan.io"
DATABOX_TYPE="databox"
DATA_DIR="${DATABOX_DATA_DIR:-/opt/datachain-rope/data}"

while [ $# -gt 0 ]; do
  case "$1" in
    --private-key) PRIVATE_KEY="$2"; shift 2 ;;
    --name) NAME="$2"; shift 2 ;;
    --type) DATABOX_TYPE="$2"; shift 2 ;;
    --region) REGION="$2"; shift 2 ;;
    --city) CITY="$2"; shift 2 ;;
    --country) COUNTRY="$2"; shift 2 ;;
    --lat) LAT="$2"; shift 2 ;;
    --lon) LON="$2"; shift 2 ;;
    --endpoint-url) ENDPOINT_URL="$2"; shift 2 ;;
    --capacity-gb) CAPACITY_GB="$2"; shift 2 ;;
    --registry-url) REGISTRY_URL="$2"; shift 2 ;;
    --data-dir) DATA_DIR="$2"; shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

: "${PRIVATE_KEY:?--private-key is required (the EVM wallet that will own this databox entry)}"
: "${NAME:?--name is required}"
: "${REGION:=global}"

if ! command -v cast >/dev/null 2>&1; then
  echo "ERROR: foundry's 'cast' is required to sign the registration message." >&2
  echo "Install with: curl -L https://foundry.paradigm.xyz | bash && foundryup" >&2
  exit 1
fi

OWNER_ADDRESS=$(cast wallet address --private-key "$PRIVATE_KEY")
TIMESTAMP=$(date +%s)

# Must byte-for-byte match register_message() in databox_registry.rs:
#   "DCROPE-DATABOX-AUTH\nregister\n{name}\n{databox_type}\n{region}\n{timestamp}"
MESSAGE=$(printf 'DCROPE-DATABOX-AUTH\nregister\n%s\n%s\n%s\n%s' "$NAME" "$DATABOX_TYPE" "$REGION" "$TIMESTAMP")

# `cast wallet sign` applies EIP-191 personal_sign encoding by default,
# which is exactly what the registry's eip191_digest() expects.
SIGNATURE=$(cast wallet sign --private-key "$PRIVATE_KEY" "$MESSAGE")

echo "Signing as owner: $OWNER_ADDRESS"
echo "Registering '$NAME' (type=$DATABOX_TYPE, region=$REGION) on $REGISTRY_URL ..."

BODY=$(python3 - "$OWNER_ADDRESS" "$NAME" "$DATABOX_TYPE" "$REGION" "${CITY:-}" "${COUNTRY:-}" "${LAT:-}" "${LON:-}" "${ENDPOINT_URL:-}" "${CAPACITY_GB:-}" "$TIMESTAMP" "$SIGNATURE" <<'PY'
import json, sys
owner, name, dtype, region, city, country, lat, lon, endpoint, cap, ts, sig = sys.argv[1:13]
payload = {
    "owner_address": owner,
    "name": name,
    "databox_type": dtype,
    "region": region,
    "timestamp": int(ts),
    "signature": sig,
}
if city: payload["city"] = city
if country: payload["country"] = country
if lat: payload["lat"] = float(lat)
if lon: payload["lon"] = float(lon)
if endpoint: payload["endpoint_url"] = endpoint
if cap: payload["capacity_gb"] = float(cap)
print(json.dumps(payload))
PY
)

RESPONSE=$(curl -sS -X POST "$REGISTRY_URL/api/v1/databoxes/register" \
  -H "content-type: application/json" \
  -d "$BODY")

echo "$RESPONSE" | python3 -m json.tool 2>/dev/null || echo "$RESPONSE"

DATABOX_ID=$(echo "$RESPONSE" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('id') or d.get('databox',{}).get('id',''))" 2>/dev/null || true)

if [ -n "${DATABOX_ID:-}" ]; then
  mkdir -p "$DATA_DIR"
  cat > "$DATA_DIR/databox.env" <<EOF
DATABOX_ID=$DATABOX_ID
DATABOX_OWNER=$OWNER_ADDRESS
DATABOX_REGISTRY_URL=$REGISTRY_URL
# WARNING: keep this file's permissions tight — anyone with the private key
# below can send heartbeats/deregister on your behalf. Prefer a dedicated
# low-value wallet for databox registration, not your treasury key.
DATABOX_PRIVATE_KEY=$PRIVATE_KEY
EOF
  chmod 600 "$DATA_DIR/databox.env"
  echo
  echo "Registered. id=$DATABOX_ID"
  echo "Saved to $DATA_DIR/databox.env for the heartbeat timer."
  echo "Enable the heartbeat with:"
  echo "  sudo systemctl enable --now databox-heartbeat.timer"
else
  echo
  echo "Registration did not return an id — check the response above." >&2
  exit 1
fi
