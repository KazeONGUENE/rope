#!/usr/bin/env bash
# P1.4 soak monitor — BLUE (or any host running datachain-rope).
# Samples RSS / threads / loopback latency / public Quipu stats vs DO1 residue
# / hang dumps / -32005 overload. No jemalloc, no DashMap try_write probe.
#
# Usage (on rope-vps):
#   ROPE_WATCHDOG_DUMP_ONLY=1  # keep for soak
#   sudo -u ubuntu bash /opt/datachain-rope/scripts/p14-soak-monitor.sh
#   # optional: INTERVAL_S=30 DURATION_S=7200 LOG=/tmp/p14-soak.log
#
set -euo pipefail

INTERVAL_S="${INTERVAL_S:-30}"
DURATION_S="${DURATION_S:-7200}"
LOG="${LOG:-/tmp/p14-soak-$(date -u +%Y%m%dT%H%MZ).log}"
RPC_LOOPBACK="${RPC_LOOPBACK:-http://127.0.0.1:8545}"
RPC_PUBLIC="${RPC_PUBLIC:-https://erpc.datachain.network}"
METRICS_URL="${METRICS_URL:-http://127.0.0.1:9090/metrics}"
DO1_RPC="${DO1_RPC:-http://157.230.18.45:8545}"
DUMP_DIR="${DUMP_DIR:-/tmp}"
PID_FILE="${PID_FILE:-}"

rpc_call() {
  local url="$1" method="$2" params="${3:-[]}"
  curl -sS --max-time 8 -X POST "$url" \
    -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":${params}}"
}

find_rope_pid() {
  if [[ -n "$PID_FILE" && -f "$PID_FILE" ]]; then
    cat "$PID_FILE"
    return
  fi
  # Prefer the systemd MainPID when available.
  if command -v systemctl >/dev/null 2>&1; then
    local main
    main="$(systemctl show -p MainPID --value datachain-rope.service 2>/dev/null || true)"
    if [[ -n "$main" && "$main" != "0" ]]; then
      echo "$main"
      return
    fi
  fi
  pgrep -xo rope 2>/dev/null || pgrep -xo datachain-rope 2>/dev/null || true
}

echo "p14-soak-monitor start utc=$(date -u +%Y-%m-%dT%H:%M:%SZ) interval=${INTERVAL_S}s duration=${DURATION_S}s log=${LOG}" | tee -a "$LOG"
echo "ts_utc pid rss_kb threads eth_ms public_strings public_knots inv do1_strings dumps_since -32005_1h metrics_rss" | tee -a "$LOG"

START_EPOCH=$(date +%s)
DUMP_BASE=$(find "$DUMP_DIR" -maxdepth 1 -name 'rope-node-hang-*' 2>/dev/null | wc -l | tr -d ' ')

while true; do
  NOW=$(date +%s)
  ELAPSED=$((NOW - START_EPOCH))
  if (( ELAPSED >= DURATION_S )); then
    echo "soak complete elapsed=${ELAPSED}s utc=$(date -u +%Y-%m-%dT%H:%M:%SZ)" | tee -a "$LOG"
    break
  fi

  TS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  PID=$(find_rope_pid)
  RSS_KB="-"
  THREADS="-"
  if [[ -n "$PID" && -r "/proc/${PID}/status" ]]; then
    RSS_KB=$(awk '/^VmRSS:/ {print $2}' "/proc/${PID}/status")
    THREADS=$(awk '/^Threads:/ {print $2}' "/proc/${PID}/status")
  fi

  T0=$(date +%s%3N)
  ETH=$(rpc_call "$RPC_LOOPBACK" eth_blockNumber '[]' 2>/dev/null || echo '{}')
  T1=$(date +%s%3N)
  ETH_MS=$((T1 - T0))

  PUB=$(rpc_call "$RPC_PUBLIC" rope_globalStats '[]' 2>/dev/null || echo '{}')
  PUB_S=$(python3 -c "import json,sys; j=json.load(sys.stdin); r=j.get('result') or {}; print(r.get('total_strings','?'), r.get('total_knots','?'), r.get('invariant_holds','?'))" <<<"$PUB" 2>/dev/null || echo '? ? ?')
  read -r PUB_STRINGS PUB_KNOTS PUB_INV <<<"$PUB_S"

  DO1_S="?"
  if DO1=$(curl -sS --max-time 4 -X POST "$DO1_RPC" \
      -H 'content-type: application/json' \
      -H 'X-Forwarded-For: soak-monitor' \
      -d '{"jsonrpc":"2.0","id":1,"method":"rope_globalStats","params":[]}' 2>/dev/null); then
    DO1_S=$(python3 -c "import json,sys; j=json.load(sys.stdin); r=j.get('result') or {}; print(r.get('total_strings','?'))" <<<"$DO1" 2>/dev/null || echo '?')
  fi

  DUMP_NOW=$(find "$DUMP_DIR" -maxdepth 1 -name 'rope-node-hang-*' 2>/dev/null | wc -l | tr -d ' ')
  DUMP_DELTA=$((DUMP_NOW - DUMP_BASE))

  OVERLOAD=$(journalctl -u datachain-rope.service --since '1 hour ago' --no-pager 2>/dev/null \
    | grep -c 'Retry-After: 1\|-32005\|OVERLOAD' || true)

  METRICS_RSS="-"
  if M=$(curl -sS --max-time 2 "$METRICS_URL" 2>/dev/null); then
    METRICS_RSS=$(echo "$M" | awk '/^process_resident_memory_bytes / {print $2; exit}')
    [[ -z "$METRICS_RSS" ]] && METRICS_RSS="-"
  fi

  # Fail-loud line if public Quipu collapsed to DO1-like residue (router failover bug)
  ALERT=""
  if [[ "$PUB_STRINGS" == "2" && "$PUB_KNOTS" == "3" ]]; then
    ALERT=" ALERT_PUBLIC_LOOKS_LIKE_DO1_RESIDUE"
  fi

  LINE="${TS} ${PID:--} ${RSS_KB} ${THREADS} ${ETH_MS} ${PUB_STRINGS} ${PUB_KNOTS} ${PUB_INV} ${DO1_S} ${DUMP_DELTA} ${OVERLOAD} ${METRICS_RSS}${ALERT}"
  echo "$LINE" | tee -a "$LOG"

  sleep "$INTERVAL_S"
done
