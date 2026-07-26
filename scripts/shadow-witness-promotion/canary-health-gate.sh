#!/usr/bin/env bash
#
# canary-health-gate.sh
#
# Evaluates the soak health of the rope-shadow-witness canary running on
# datachain-rpc-1 (DigitalOcean tertiary, Frankfurt) against a fixed set of
# success criteria, prints a structured report, and exits 0 (PASS) or 1 (FAIL).
#
# Designed to run on rope-vps (BLUE), which has SSH access to the canary
# (`root@157.230.18.45`, key `~/.ssh/datachain_rope_id_rsa`).
#
# Usage:
#   canary-health-gate.sh                 # full gate, exit 1 on fail
#   canary-health-gate.sh --report-only   # always exit 0, just print report
#   canary-health-gate.sh --json          # emit a single JSON object on stdout
#
# Soak success criteria (Quipu Canon §6.1.1, runbook §6):
#   - service.active                          must be true
#   - chain.first_observed_at_age_s           must be >= 7 days (604800 s)
#                                             [data-derived soak; survives
#                                              binary refresh, unlike systemd
#                                              ActiveEnterTimestamp]
#   - rounds.last_hour.failure_pct            must be <= 5
#   - logs.last_24h.error_count               must be <= 50
#   - chain.observed_strings                  must be >= 1
#   - chain.observed_knots                    must be >= 1
#   - rpc.local_status_ok                     must be true (rope_v2_status responds)
#   - chain.last_observed_at_age_s            must be <= 60 (witness is keeping up)
#   - process.rss_kb                          must be < 524288 (512 MB)
#
# All criteria are pass/fail per row; the gate passes only if every row passes.

set -euo pipefail

# ---------------------------------------------------------------------------
# Config
# ---------------------------------------------------------------------------
CANARY_HOST="${CANARY_HOST:-root@157.230.18.45}"
CANARY_KEY="${CANARY_KEY:-$HOME/.ssh/datachain_rope_id_rsa}"
CANARY_RPC_LOCAL="${CANARY_RPC_LOCAL:-http://127.0.0.1:8556}"
SOAK_DAYS="${SOAK_DAYS:-7}"
SOAK_SECS=$(( SOAK_DAYS * 86400 ))
MAX_FAIL_PCT="${MAX_FAIL_PCT:-5}"
MAX_ERRORS_24H="${MAX_ERRORS_24H:-50}"
MAX_OBS_AGE_S="${MAX_OBS_AGE_S:-60}"
MAX_RSS_KB="${MAX_RSS_KB:-524288}"

REPORT_ONLY=false
EMIT_JSON=false
for arg in "$@"; do
    case "$arg" in
        --report-only) REPORT_ONLY=true ;;
        --json) EMIT_JSON=true ;;
        --help|-h)
            sed -n '3,30p' "$0"; exit 0 ;;
        *) echo "unknown arg: $arg" >&2; exit 2 ;;
    esac
done

ssh_canary() {
    ssh -o BatchMode=yes -o ConnectTimeout=10 -o StrictHostKeyChecking=accept-new \
        -i "$CANARY_KEY" "$CANARY_HOST" "$@"
}

# ---------------------------------------------------------------------------
# Probe canary (one round-trip via SSH; everything below runs remotely)
# ---------------------------------------------------------------------------
PROBE=$(ssh_canary 'bash -s' <<'REMOTE'
set -uo pipefail

# Service state
ACTIVE=$(systemctl is-active rope-shadow-witness 2>/dev/null || echo "unknown")
ENABLED=$(systemctl is-enabled rope-shadow-witness 2>/dev/null || echo "unknown")

# Uptime in seconds (since last ActiveEnterTimestamp)
START_TS=$(systemctl show rope-shadow-witness --property=ActiveEnterTimestamp --value 2>/dev/null || echo "")
if [ -n "$START_TS" ] && [ "$START_TS" != "n/a" ]; then
    START_EPOCH=$(date -d "$START_TS" +%s 2>/dev/null || echo 0)
    UPTIME_S=$(( $(date +%s) - START_EPOCH ))
else
    UPTIME_S=0
fi

# Rounds in last hour: count occurrences and accumulate wallets_failed.
# Strip ANSI escape sequences first (tracing-subscriber emits them by
# default even with `-o cat`); without this, "wallets" and "=" are
# separated by `\x1b[0m\x1b[2m` and the regex never matches.
SINCE="1 hour ago"
LOG_HOUR=$(journalctl -u rope-shadow-witness --since "$SINCE" --no-pager -o cat 2>/dev/null \
    | sed -E 's/\x1B\[[0-9;]*[a-zA-Z]//g' || true)
ROUNDS=$(printf '%s\n' "$LOG_HOUR" | grep -c "round complete" 2>/dev/null || echo 0)
WALLETS_TOTAL=$(printf '%s\n' "$LOG_HOUR" | grep -oE 'wallets=[0-9]+' | awk -F= '{s+=$2} END{print s+0}')
WALLETS_FAILED=$(printf '%s\n' "$LOG_HOUR" | grep -oE 'wallets_failed=[0-9]+' | awk -F= '{s+=$2} END{print s+0}')
KNOTS_APPLIED=$(printf '%s\n' "$LOG_HOUR" | grep -oE 'knots_applied=[0-9]+' | awk -F= '{s+=$2} END{print s+0}')

# Errors in last 24h (priority "err" or higher in journald)
ERRORS_24H=$(journalctl -u rope-shadow-witness --since "24 hours ago" -p err --no-pager 2>/dev/null | wc -l | tr -d ' ')

# Local RPC status
RPC_STATUS=$(curl -sS --max-time 5 -X POST -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"rope_v2_status","params":[],"id":1}' \
    http://127.0.0.1:8556 2>/dev/null || echo '{"error":"unreachable"}')

# Resident set size of the witness process (KB)
PID=$(systemctl show rope-shadow-witness --property=MainPID --value 2>/dev/null || echo 0)
if [ "$PID" -gt 0 ] && [ -d "/proc/$PID" ]; then
    RSS_KB=$(awk '/VmRSS/ {print $2}' /proc/$PID/status 2>/dev/null || echo 0)
else
    RSS_KB=0
fi

# Disk usage of the chain store
DATA_DIR_KB=$(du -s /var/lib/rope-shadow-witness/data 2>/dev/null | awk '{print $1}' || echo 0)

# Now the structured emission for the parent to consume.
echo "=== PROBE_BEGIN ==="
echo "ACTIVE=$ACTIVE"
echo "ENABLED=$ENABLED"
echo "UPTIME_S=$UPTIME_S"
echo "ROUNDS=$ROUNDS"
echo "WALLETS_TOTAL=$WALLETS_TOTAL"
echo "WALLETS_FAILED=$WALLETS_FAILED"
echo "KNOTS_APPLIED=$KNOTS_APPLIED"
echo "ERRORS_24H=$ERRORS_24H"
echo "RSS_KB=$RSS_KB"
echo "DATA_DIR_KB=$DATA_DIR_KB"
echo "PID=$PID"
echo "RPC_STATUS_BEGIN"
echo "$RPC_STATUS"
echo "RPC_STATUS_END"
echo "=== PROBE_END ==="
REMOTE
)

# ---------------------------------------------------------------------------
# Parse the probe blob (locally on BLUE)
# ---------------------------------------------------------------------------
declare -A P
while IFS='=' read -r k v; do
    case "$k" in
        ACTIVE|ENABLED|UPTIME_S|ROUNDS|WALLETS_TOTAL|WALLETS_FAILED|KNOTS_APPLIED|ERRORS_24H|RSS_KB|DATA_DIR_KB|PID)
            P[$k]="$v" ;;
    esac
done <<<"$(printf '%s\n' "$PROBE" | sed -n '/=== PROBE_BEGIN ===/,/=== PROBE_END ===/p' | grep -E '^[A-Z][A-Z0-9_]*=' || true)"

RPC_BODY=$(printf '%s\n' "$PROBE" | sed -n '/^RPC_STATUS_BEGIN$/,/^RPC_STATUS_END$/p' | sed '1d;$d')

# Default everything to 0/false on parse miss
ACTIVE="${P[ACTIVE]:-unknown}"
UPTIME_S="${P[UPTIME_S]:-0}"
ROUNDS="${P[ROUNDS]:-0}"
WALLETS_TOTAL="${P[WALLETS_TOTAL]:-0}"
WALLETS_FAILED="${P[WALLETS_FAILED]:-0}"
KNOTS_APPLIED="${P[KNOTS_APPLIED]:-0}"
ERRORS_24H="${P[ERRORS_24H]:-9999}"
RSS_KB="${P[RSS_KB]:-0}"
DATA_DIR_KB="${P[DATA_DIR_KB]:-0}"

# Failure percentage (integer arithmetic, safe when total is 0)
if [ "$WALLETS_TOTAL" -gt 0 ]; then
    FAIL_PCT=$(( WALLETS_FAILED * 100 / WALLETS_TOTAL ))
else
    FAIL_PCT=0
fi

# Pull observed_strings, observed_knots, last_observed_at from the RPC body.
# The witness exposes these via `rope_v2_status` (Server::dispatch in rope-shadow-witness).
OBS_STRINGS=$(printf '%s' "$RPC_BODY" | python3 -c 'import sys,json
try:
    j = json.loads(sys.stdin.read())
    r = j.get("result", {}) or {}
    print(r.get("observed_strings", 0))
except Exception: print(0)' 2>/dev/null || echo 0)
OBS_KNOTS=$(printf '%s' "$RPC_BODY" | python3 -c 'import sys,json
try:
    j = json.loads(sys.stdin.read())
    r = j.get("result", {}) or {}
    print(r.get("observed_knots", 0))
except Exception: print(0)' 2>/dev/null || echo 0)
LAST_OBS_AGE_S=$(printf '%s' "$RPC_BODY" | python3 -c 'import sys,json,time
try:
    j = json.loads(sys.stdin.read())
    r = j.get("result", {}) or {}
    last = int(r.get("last_observed_at_unix", 0))
    if last > 0:
        print(int(time.time()) - last)
    else:
        print(99999)
except Exception: print(99999)' 2>/dev/null || echo 99999)
FIRST_OBS_AGE_S=$(printf '%s' "$RPC_BODY" | python3 -c 'import sys,json,time
try:
    j = json.loads(sys.stdin.read())
    r = j.get("result", {}) or {}
    # Prefer the immutable install marker (set once on first install,
    # never overwritten), fall back to store-derived first_observed only
    # if the marker is absent (e.g. on canary versions < 0.1.2).
    install = int(r.get("first_install_at_unix", 0))
    first = int(r.get("first_observed_at_unix", 0))
    chosen = install if install > 0 else first
    if chosen > 0:
        print(int(time.time()) - chosen)
    else:
        print(0)
except Exception: print(0)' 2>/dev/null || echo 0)
RPC_LOCAL_OK=$(printf '%s' "$RPC_BODY" | python3 -c 'import sys,json
try:
    j = json.loads(sys.stdin.read())
    print("true" if j.get("result") is not None else "false")
except Exception: print("false")' 2>/dev/null || echo false)

# ---------------------------------------------------------------------------
# Apply criteria
# ---------------------------------------------------------------------------
PASS_OVERALL=true
declare -a ROWS

check() {
    local label="$1"
    local actual="$2"
    local op="$3"
    local threshold="$4"
    local pass

    case "$op" in
        eq)  [ "$actual" = "$threshold" ] && pass=true || pass=false ;;
        ge)  [ "$actual" -ge "$threshold" ] && pass=true || pass=false ;;
        le)  [ "$actual" -le "$threshold" ] && pass=true || pass=false ;;
        lt)  [ "$actual" -lt "$threshold" ] && pass=true || pass=false ;;
        *)   pass=false ;;
    esac
    if ! $pass; then PASS_OVERALL=false; fi
    ROWS+=("$(printf "  %-32s actual=%-12s op=%s threshold=%-12s -> %s" "$label" "$actual" "$op" "$threshold" "$( $pass && echo PASS || echo FAIL )")")
}

check "service.active"                "$ACTIVE"        eq "active"
check "chain.first_observed_at_age_s" "$FIRST_OBS_AGE_S" ge "$SOAK_SECS"
check "rounds.last_hour.failure_pct"  "$FAIL_PCT"      le "$MAX_FAIL_PCT"
check "logs.last_24h.error_count"     "$ERRORS_24H"    le "$MAX_ERRORS_24H"
check "rpc.local_status_ok"           "$RPC_LOCAL_OK"  eq "true"
check "chain.observed_strings"        "$OBS_STRINGS"   ge 1
check "chain.observed_knots"          "$OBS_KNOTS"     ge 1
check "chain.last_observed_at_age_s"  "$LAST_OBS_AGE_S" le "$MAX_OBS_AGE_S"
check "process.rss_kb"                "$RSS_KB"        lt "$MAX_RSS_KB"

# ---------------------------------------------------------------------------
# Emit
# ---------------------------------------------------------------------------
if $EMIT_JSON; then
    cat <<JSON
{
  "ts_utc": "$(date -u +%Y-%m-%dT%H:%M:%SZ)",
  "soak_days_required": $SOAK_DAYS,
  "service_active": "$ACTIVE",
  "service_uptime_s": $UPTIME_S,
  "first_observed_at_age_s": $FIRST_OBS_AGE_S,
  "rounds_last_hour": $ROUNDS,
  "wallets_total_last_hour": $WALLETS_TOTAL,
  "wallets_failed_last_hour": $WALLETS_FAILED,
  "knots_applied_last_hour": $KNOTS_APPLIED,
  "failure_pct_last_hour": $FAIL_PCT,
  "errors_24h": $ERRORS_24H,
  "rss_kb": $RSS_KB,
  "data_dir_kb": $DATA_DIR_KB,
  "observed_strings": $OBS_STRINGS,
  "observed_knots": $OBS_KNOTS,
  "last_observed_at_age_s": $LAST_OBS_AGE_S,
  "rpc_local_ok": $RPC_LOCAL_OK,
  "gate_pass": $( $PASS_OVERALL && echo true || echo false )
}
JSON
else
    echo "============================================================"
    echo "rope-shadow-witness canary health gate"
    echo "  ts:           $(date -u +%Y-%m-%dT%H:%M:%SZ)"
    echo "  canary:       $CANARY_HOST"
    echo "  soak_required: ${SOAK_DAYS} days (${SOAK_SECS} s)"
    echo "------------------------------------------------------------"
    for r in "${ROWS[@]}"; do echo "$r"; done
    echo "------------------------------------------------------------"
    if $PASS_OVERALL; then
        echo "GATE_RESULT: PASS"
    else
        echo "GATE_RESULT: FAIL"
    fi
    echo "============================================================"
fi

if $REPORT_ONLY; then exit 0; fi
$PASS_OVERALL && exit 0 || exit 1
