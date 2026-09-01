#!/usr/bin/env bash
# Regression tests for read_external_probes() in erpc-fleet-ha.sh
#
# Runs the aggregator against synthetic ndjson inputs and asserts the output
# JSON shape + escalation logic match the EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1
# spec (datachain-rope/docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md).
#
# Usage: bash deploy/scripts/tests/test-read-external-probes.sh
#
# Exits 0 on all-green, 1 on any assertion failure.

set -u -o pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
HA_SCRIPT="$SCRIPT_DIR/erpc-fleet-ha.sh"

if [[ ! -f "$HA_SCRIPT" ]]; then
  echo "FATAL: cannot find $HA_SCRIPT" >&2
  exit 2
fi

if ! command -v python3 >/dev/null 2>&1; then
  echo "SKIP: python3 not available" >&2
  exit 0
fi

if ! command -v jq >/dev/null 2>&1; then
  echo "SKIP: jq not available" >&2
  exit 0
fi

# Extract the read_external_probes() function + its env-var defaults so we can
# exercise it in isolation. Line ranges match the current erpc-fleet-ha.sh
# structure (function lives at ~line 395, defaults at ~line 59).
DEFAULTS_START=$(grep -n '^EDGE_EXTERNAL_NDJSON=' "$HA_SCRIPT" | head -1 | cut -d: -f1)
DEFAULTS_END=$(grep -n '^EDGE_EXTERNAL_MAX_LINES=' "$HA_SCRIPT" | head -1 | cut -d: -f1)
FN_START=$(grep -n '^read_external_probes()' "$HA_SCRIPT" | head -1 | cut -d: -f1)
# The naive `^}$` match will trip on a `}` inside the embedded Python heredoc.
# Instead, find the NEXT top-level function definition after read_external_probes
# and walk backwards until we hit the closing brace.
NEXT_FN=$(awk -v s="$FN_START" 'NR>s && /^[a-z_]+\(\)/ { print NR; exit }' "$HA_SCRIPT")
if [[ -n "$NEXT_FN" ]]; then
  FN_END=$(awk -v s="$FN_START" -v e="$NEXT_FN" 'NR>=s && NR<e && /^}$/ { last=NR } END { print last }' "$HA_SCRIPT")
fi

if [[ -z "$DEFAULTS_START" || -z "$DEFAULTS_END" || -z "$FN_START" || -z "$FN_END" ]]; then
  echo "FATAL: could not locate function or defaults inside $HA_SCRIPT" >&2
  echo "  DEFAULTS_START=$DEFAULTS_START DEFAULTS_END=$DEFAULTS_END FN_START=$FN_START FN_END=$FN_END NEXT_FN=$NEXT_FN" >&2
  exit 2
fi

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

AGG="$TMP/agg.sh"
{
  echo "#!/usr/bin/env bash"
  echo "set -u"
  sed -n "${DEFAULTS_START},${DEFAULTS_END}p" "$HA_SCRIPT"
  echo 'EDGE_EXTERNAL_DEGRADED_SINCE=${EDGE_EXTERNAL_DEGRADED_SINCE:-0}'
  sed -n "${FN_START},${FN_END}p" "$HA_SCRIPT"
  echo
  echo "read_external_probes"
} > "$AGG"

if ! bash -n "$AGG"; then
  echo "FATAL: extracted aggregator failed shell syntax check" >&2
  exit 2
fi

PASS=0
FAIL=0

assert_eq() {
  local label="$1" expected="$2" actual="$3"
  if [[ "$expected" == "$actual" ]]; then
    printf '  PASS  %s\n' "$label"
    PASS=$((PASS + 1))
  else
    printf '  FAIL  %s\n        expected=%s actual=%s\n' "$label" "$expected" "$actual"
    FAIL=$((FAIL + 1))
  fi
}

run_case() {
  local name="$1" ndjson="$2" degraded_since_env="${3:-0}"
  # Header goes to stderr so stdout is pure JSON (jq input).
  echo "== $name ==" >&2
  ROPE_HA_EDGE_EXTERNAL_FILE="$ndjson" \
    EDGE_EXTERNAL_DEGRADED_SINCE="$degraded_since_env" \
    bash "$AGG"
}

NOW=$(date +%s)

# ---------- Case 1: ingest file absent ----------
CASE1_OUT=$(run_case "no ingest file" "$TMP/does-not-exist.ndjson")
assert_eq "case1 enabled=false" "false" "$(printf '%s' "$CASE1_OUT" | jq -r '.enabled')"
assert_eq "case1 reason=no_ingest_file" "no_ingest_file" "$(printf '%s' "$CASE1_OUT" | jq -r '.reason')"
assert_eq "case1 peers empty" "0" "$(printf '%s' "$CASE1_OUT" | jq -r '.peers | length')"

# ---------- Case 2: empty file ----------
: > "$TMP/empty.ndjson"
CASE2_OUT=$(run_case "empty ingest file" "$TMP/empty.ndjson")
assert_eq "case2 enabled=true" "true" "$(printf '%s' "$CASE2_OUT" | jq -r '.enabled')"
assert_eq "case2 peer_count=0" "0" "$(printf '%s' "$CASE2_OUT" | jq -r '.peer_count')"
assert_eq "case2 escalate=false" "false" "$(printf '%s' "$CASE2_OUT" | jq -r '.escalate')"

# ---------- Case 3: healthy fleet (below threshold) ----------
cat > "$TMP/healthy.ndjson" <<EOF
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-dcswap","peer_source_region":"eu-paris-sd6","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":100,"sample_fail":0,"fail_ratio":0.0,"reasons":{}}}
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-tanastok","peer_source_region":"eu-paris-sd6","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":98,"sample_fail":2,"fail_ratio":0.02,"reasons":{"http_502":2}}}
EOF
CASE3_OUT=$(run_case "healthy fleet (0% + 2% fail)" "$TMP/healthy.ndjson")
assert_eq "case3 peer_count=2" "2" "$(printf '%s' "$CASE3_OUT" | jq -r '.peer_count')"
assert_eq "case3 degraded_peer_count=0" "0" "$(printf '%s' "$CASE3_OUT" | jq -r '.degraded_peer_count')"
assert_eq "case3 escalate=false" "false" "$(printf '%s' "$CASE3_OUT" | jq -r '.escalate')"

# ---------- Case 4: one degraded peer, min_peers=2, no escalate ----------
cat > "$TMP/one-bad.ndjson" <<EOF
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-dcswap","peer_source_region":"eu-paris-sd6","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":100,"sample_fail":0,"fail_ratio":0.0,"reasons":{}}}
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-tanastok","peer_source_region":"eu-paris-sd6","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":100,"sample_fail":0,"fail_ratio":0.0,"reasons":{}}}
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-alteros","peer_source_region":"eu-fra","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":75,"sample_fail":25,"fail_ratio":0.25,"reasons":{"http_502":25}}}
EOF
CASE4_OUT=$(run_case "single degraded peer (below min_peers)" "$TMP/one-bad.ndjson" "$((NOW - 500))")
assert_eq "case4 peer_count=3" "3" "$(printf '%s' "$CASE4_OUT" | jq -r '.peer_count')"
assert_eq "case4 degraded_peer_count=1" "1" "$(printf '%s' "$CASE4_OUT" | jq -r '.degraded_peer_count')"
assert_eq "case4 escalate=false (below min_peers)" "false" "$(printf '%s' "$CASE4_OUT" | jq -r '.escalate')"

# ---------- Case 5: two degraded, sustained > 180s, escalate=true ----------
cat > "$TMP/two-bad.ndjson" <<EOF
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-dcswap","peer_source_region":"eu-paris-sd6","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":100,"sample_fail":0,"fail_ratio":0.0,"reasons":{}}}
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-tanastok","peer_source_region":"eu-paris-sd6","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":80,"sample_fail":20,"fail_ratio":0.20,"reasons":{"http_502":20}}}
{"received_at":$NOW,"body":{"schema":"datachain.edge-probe-report/v1","peer_id":"cerber-alteros","peer_source_region":"eu-fra","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":75,"sample_fail":25,"fail_ratio":0.25,"reasons":{"http_502":15,"timeout":10}}}
EOF
CASE5_OUT=$(run_case "two degraded peers sustained 500s" "$TMP/two-bad.ndjson" "$((NOW - 500))")
assert_eq "case5 degraded_peer_count=2" "2" "$(printf '%s' "$CASE5_OUT" | jq -r '.degraded_peer_count')"
assert_eq "case5 degraded_for_secs>=180" "true" "$([ "$(printf '%s' "$CASE5_OUT" | jq -r '.degraded_for_secs')" -ge 180 ] && echo true || echo false)"
assert_eq "case5 escalate=true" "true" "$(printf '%s' "$CASE5_OUT" | jq -r '.escalate')"

# ---------- Case 6: two degraded but NOT sustained (degraded_since=0) ----------
CASE6_OUT=$(run_case "two degraded peers not yet sustained" "$TMP/two-bad.ndjson" "0")
assert_eq "case6 degraded_peer_count=2" "2" "$(printf '%s' "$CASE6_OUT" | jq -r '.degraded_peer_count')"
# degraded_since is set NOW; degraded_for_secs is small; escalate must be false
assert_eq "case6 escalate=false (window just opened)" "false" "$(printf '%s' "$CASE6_OUT" | jq -r '.escalate')"

# ---------- Case 7: malformed + stale filtering ----------
STALE=$((NOW - 4000))
cat > "$TMP/mixed.ndjson" <<EOF
not-json-at-all
{"received_at":$STALE,"body":{"peer_id":"cerber-old","target_url":"https://erpc.datachain.network","window_start":$((STALE-60)),"window_end":$STALE,"window_secs":60,"sample_n":100,"sample_ok":50,"sample_fail":50,"fail_ratio":0.5}}
{"received_at":$NOW,"body":{"peer_id":"cerber-fresh","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":100,"sample_ok":100,"sample_fail":0,"fail_ratio":0.0}}
EOF
CASE7_OUT=$(run_case "malformed + stale filtering" "$TMP/mixed.ndjson")
assert_eq "case7 malformed_lines=1" "1" "$(printf '%s' "$CASE7_OUT" | jq -r '.malformed_lines')"
assert_eq "case7 peer_count=1 (stale filtered)" "1" "$(printf '%s' "$CASE7_OUT" | jq -r '.peer_count')"
assert_eq "case7 peer=cerber-fresh" "cerber-fresh" "$(printf '%s' "$CASE7_OUT" | jq -r '.peers[0].peer_id')"

# ---------- Case 8: multiple reports per peer aggregated ----------
cat > "$TMP/multi-report.ndjson" <<EOF
{"received_at":$((NOW-120)),"body":{"peer_id":"cerber-dcswap","target_url":"https://erpc.datachain.network","window_start":$((NOW-180)),"window_end":$((NOW-120)),"window_secs":60,"sample_n":50,"sample_ok":45,"sample_fail":5,"fail_ratio":0.10,"reasons":{"http_502":5}}}
{"received_at":$((NOW-60)),"body":{"peer_id":"cerber-dcswap","target_url":"https://erpc.datachain.network","window_start":$((NOW-120)),"window_end":$((NOW-60)),"window_secs":60,"sample_n":50,"sample_ok":50,"sample_fail":0,"fail_ratio":0.0,"reasons":{}}}
{"received_at":$NOW,"body":{"peer_id":"cerber-dcswap","target_url":"https://erpc.datachain.network","window_start":$((NOW-60)),"window_end":$NOW,"window_secs":60,"sample_n":50,"sample_ok":48,"sample_fail":2,"fail_ratio":0.04,"reasons":{"timeout":2}}}
EOF
CASE8_OUT=$(run_case "3 reports aggregated for 1 peer" "$TMP/multi-report.ndjson")
assert_eq "case8 peer_count=1" "1" "$(printf '%s' "$CASE8_OUT" | jq -r '.peer_count')"
assert_eq "case8 sample_n aggregated=150" "150" "$(printf '%s' "$CASE8_OUT" | jq -r '.peers[0].sample_n')"
assert_eq "case8 sample_fail aggregated=7" "7" "$(printf '%s' "$CASE8_OUT" | jq -r '.peers[0].sample_fail')"
assert_eq "case8 reports=3" "3" "$(printf '%s' "$CASE8_OUT" | jq -r '.peers[0].reports')"
assert_eq "case8 reasons.http_502=5" "5" "$(printf '%s' "$CASE8_OUT" | jq -r '.peers[0].reasons.http_502')"
assert_eq "case8 reasons.timeout=2" "2" "$(printf '%s' "$CASE8_OUT" | jq -r '.peers[0].reasons.timeout')"

echo
echo "----------------------------------------"
if [[ $FAIL -eq 0 ]]; then
  printf 'ALL %d assertions passed.\n' "$PASS"
  exit 0
else
  printf '%d passed / %d FAILED.\n' "$PASS" "$FAIL"
  exit 1
fi
