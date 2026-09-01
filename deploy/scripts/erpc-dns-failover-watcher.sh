#!/usr/bin/env bash
# erpc-dns-failover-watcher.sh — Option 3 from May-20 DNS SPOF plan.
#
# MUST run on a host that is NOT BLUE (GREEN or DO rpc-1 recommended).
# When BLUE's public edge is unreachable for FAIL_THRESHOLD cycles, rewrite
# Gandi LiveDNS A records for erpc (and optional companions) to GREEN.
# When BLUE recovers, rewrite back.
#
# Env (required on the watcher host):
#   GANDI_API_KEY          — LiveDNS API key
#   GANDI_DNS_DOMAIN       — default datachain.network
# Optional:
#   ROPE_DNS_BLUE_IP       — default 159.65.208.206 (London writer, ISO 2026-08-24)
#   ROPE_DNS_FAILOVER_IP   — default 157.230.18.45 (DO rpc-1; terminates erpc TLS)
#   ROPE_DNS_NAMES         — space-separated relative names (default: erpc)
#   ROPE_DNS_PROBE_URL     — default https://erpc.datachain.network/healthz
#
# Live 2026-07-28: GREEN :443 does not terminate erpc (timeout). DO rpc-1
# does (healthz 200). Failover A-record target defaults to DO rpc-1.
set -euo pipefail

LOG="${ROPE_DNS_FAILOVER_LOG:-/var/log/erpc-dns-failover.log}"
STATE="${ROPE_DNS_FAILOVER_STATE:-/var/lib/datachain-rope/fleet/dns-failover.state}"
API_KEY="${GANDI_API_KEY:-}"
DOMAIN="${GANDI_DNS_DOMAIN:-datachain.network}"
BLUE_IP="${ROPE_DNS_BLUE_IP:-159.65.208.206}"
FAILOVER_IP="${ROPE_DNS_FAILOVER_IP:-157.230.18.45}"
NAMES="${ROPE_DNS_NAMES:-erpc}"
PROBE_URL="${ROPE_DNS_PROBE_URL:-https://erpc.datachain.network/healthz}"
# Also probe BLUE IP directly (bypasses DNS cache) when dig/curl to IP:443
BLUE_DIRECT="${ROPE_DNS_BLUE_DIRECT:-https://159.65.208.206/healthz}"
FAIL_THRESHOLD="${ROPE_DNS_FAIL_THRESHOLD:-3}"
TIMEOUT_S="${ROPE_DNS_PROBE_TIMEOUT_S:-5}"

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }
mkdir -p "$(dirname "$STATE")" "$(dirname "$LOG")" 2>/dev/null || true

if [[ -z "$API_KEY" && -f /etc/datachain-rope.env ]]; then
  # shellcheck disable=SC1091
  set -a; source /etc/datachain-rope.env; set +a
  API_KEY="${GANDI_API_KEY:-}"
fi
if [[ -z "$API_KEY" ]]; then
  echo "$(ts) FATAL: GANDI_API_KEY unset — DNS watcher idle" >>"$LOG"
  exit 0
fi

FAIL_COUNT=0
ACTIVE_TARGET=blue
if [[ -f "$STATE" ]]; then
  # shellcheck disable=SC1090
  source "$STATE" 2>/dev/null || true
fi

probe_ok() {
  local url="$1"
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$TIMEOUT_S" \
    -k --resolve "erpc.datachain.network:443:${BLUE_IP}" \
    "https://erpc.datachain.network/healthz" 2>/dev/null || echo 000)
  # Prefer SNI resolve to BLUE IP so we don't depend on public DNS.
  if [[ "$url" == "blue" ]]; then
    [[ "$code" == "200" ]]
    return
  fi
  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$TIMEOUT_S" "$PROBE_URL" 2>/dev/null || echo 000)
  [[ "$code" == "200" ]]
}

blue_edge_ok() {
  local code
  code=$(curl -sS -o /dev/null -w '%{http_code}' --max-time "$TIMEOUT_S" \
    --resolve "erpc.datachain.network:443:${BLUE_IP}" \
    "https://erpc.datachain.network/healthz" 2>/dev/null || echo 000)
  [[ "$code" == "200" ]]
}

gandi_set_a() {
  local name="$1" ip="$2"
  local url="https://api.gandi.net/v5/livedns/domains/${DOMAIN}/records/${name}/A"
  curl -sS --max-time 15 -X PUT "$url" \
    -H "Authorization: Bearer ${API_KEY}" \
    -H "Content-Type: application/json" \
    -d "{\"rrset_values\":[\"${ip}\"],\"rrset_ttl\":300}" >>"$LOG" 2>&1
  echo >>"$LOG"
}

save() {
  cat >"$STATE" <<EOF
FAIL_COUNT=${FAIL_COUNT}
ACTIVE_TARGET=${ACTIVE_TARGET}
EOF
}

if blue_edge_ok; then
  if [[ "$FAIL_COUNT" -ne 0 ]]; then
    echo "$(ts) BLUE edge recovered" >>"$LOG"
  fi
  FAIL_COUNT=0
  if [[ "$ACTIVE_TARGET" != "blue" ]]; then
    echo "$(ts) DNS failover REVERT → ${BLUE_IP}" >>"$LOG"
    for n in $NAMES; do
      gandi_set_a "$n" "$BLUE_IP" || true
    done
    ACTIVE_TARGET=blue
  else
    # Heartbeat every successful probe (proves watcher is alive).
    echo "$(ts) ok target=blue blue_edge=up" >>"$LOG"
  fi
  save
  exit 0
fi

FAIL_COUNT=$((FAIL_COUNT + 1))
echo "$(ts) BLUE edge probe fail count=${FAIL_COUNT}/${FAIL_THRESHOLD}" >>"$LOG"
save

if [[ "$FAIL_COUNT" -ge "$FAIL_THRESHOLD" && "$ACTIVE_TARGET" != "green" ]]; then
  echo "$(ts) DNS failover PROMOTE → ${FAILOVER_IP} names=${NAMES}" >>"$LOG"
  for n in $NAMES; do
    gandi_set_a "$n" "$FAILOVER_IP" || true
  done
  ACTIVE_TARGET=green
  save
fi

exit 0
