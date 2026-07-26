#!/usr/bin/env bash
# Sends one signed liveness heartbeat to the Global Databox Network.
# Run periodically by databox-heartbeat.timer (every 5 min; server TTL 10 min).
set -euo pipefail

DATA_DIR="${DATABOX_DATA_DIR:-/opt/datachain-rope/data}"
ENV_FILE="$DATA_DIR/databox.env"

if [ ! -f "$ENV_FILE" ]; then
  echo "No $ENV_FILE — run scripts/register-databox.sh first. Skipping heartbeat." >&2
  exit 0
fi
# shellcheck disable=SC1090
source "$ENV_FILE"

: "${DATABOX_ID:?missing in $ENV_FILE}"
: "${DATABOX_PRIVATE_KEY:?missing in $ENV_FILE}"
: "${DATABOX_REGISTRY_URL:=https://dcscan.io}"

if ! command -v cast >/dev/null 2>&1; then
  echo "ERROR: foundry's 'cast' is required to sign the heartbeat." >&2
  exit 1
fi

TIMESTAMP=$(date +%s)
# Must byte-for-byte match heartbeat_message() in databox_registry.rs:
#   "DCROPE-DATABOX-AUTH\nheartbeat\n{id}\n{timestamp}"
MESSAGE=$(printf 'DCROPE-DATABOX-AUTH\nheartbeat\n%s\n%s' "$DATABOX_ID" "$TIMESTAMP")
SIGNATURE=$(cast wallet sign --private-key "$DATABOX_PRIVATE_KEY" "$MESSAGE")

BODY=$(python3 -c "import json,sys; print(json.dumps({'timestamp': int(sys.argv[1]), 'signature': sys.argv[2]}))" "$TIMESTAMP" "$SIGNATURE")

curl -sS -X POST "$DATABOX_REGISTRY_URL/api/v1/databoxes/$DATABOX_ID/heartbeat" \
  -H "content-type: application/json" \
  -d "$BODY" -o /dev/null -w "heartbeat -> HTTP %{http_code}\n"
