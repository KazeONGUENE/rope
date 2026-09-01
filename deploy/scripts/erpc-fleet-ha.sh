#!/usr/bin/env bash
# erpc-fleet-ha.sh - autonomous detect → heal for the Rope RPC fleet.
#
# Runs every 30s (systemd timer or cron). Lives outside the suspect process.
# Acceptance targets (DCSwap handover 2026-07-28):
#   detect ≤30s · read drain via nginx backups · sick-node restart ≤2m
#
# Writer promote to GREEN for eth_sendRawTransaction is intentionally NOT
# done here: committee followers do not build blocks from a foreign mempool
# (2026-07-25 silent-unmined incident). Quipu rope_* is BLUE-local. Heal
# path = forensics + restart BLUE rope-node; nginx already fails eth_* reads
# over to GREEN/DO when BLUE times out.
#
# Ghost reclaim (2026-07-29): if a signed eth_sendRawTransaction lands on an
# attester (DO1/GREEN/DO2) but never on BLUE, it sits forever unmined while
# public reads intermittently show "pending". Every healthy tick this script
# diffs peer txpools vs BLUE, pulls eth_getRawTransactionByHash from the
# peer that holds the ghost, and re-injects into BLUE - same recovery that
# mined the 500M FATMigrationMinter escrow fund, without human SSH.
#
# Publishes: /var/lib/datachain-rope/fleet/fleet-status.json
#            (served publicly at https://erpc.datachain.network/v1/fleet-status)
set -euo pipefail

STATE_DIR="${ROPE_FLEET_STATE_DIR:-/var/lib/datachain-rope/fleet}"
# Public JSON is written into the nginx html bind-mount so rope-nginx can
# serve it without a container recreate (see location = /v1/fleet-status).
STATUS_FILE="${ROPE_FLEET_STATUS_FILE:-/opt/datachain-rope/code/deploy/nginx/html/fleet/fleet-status.json}"
STATE_FILE="${STATE_DIR}/ha.state"
LOG="${ROPE_FLEET_HA_LOG:-/var/log/erpc-fleet-ha.log}"
BLUE_RPC="${ROPE_BLUE_RPC:-http://127.0.0.1:8545}"
# When set (e.g. http://127.0.0.1:8544/v1/tip), loopback health uses the sync
# probe thread instead of JSON-RPC on :8545 — avoids false rpc_probe_fail when
# the Tokio handler pool is saturated but the process is alive.
ROPE_BLUE_PROBE_URL="${ROPE_BLUE_PROBE_URL:-}"
PEER_RPCS="${ROPE_PEER_RPCS:-http://92.243.25.119:8545 http://157.230.18.45:8545 http://167.172.106.174:8545}"
PUBLIC_EDGE_URL="${ROPE_PUBLIC_EDGE_URL:-https://erpc.datachain.network}"
PUBLIC_EDGE_HOST="${ROPE_PUBLIC_EDGE_HOST:-erpc.datachain.network}"
# Public-edge sample (DCSwap ask 2026-07-28): writer.local healthy ≠ edge OK.
EDGE_SAMPLE_N="${ROPE_HA_EDGE_SAMPLE_N:-10}"
EDGE_FAIL_RATIO="${ROPE_HA_EDGE_FAIL_RATIO:-0.4}"   # > this → edge degraded/down
# Require this many consecutive bad ticks before publishing edge=degraded
# (avoids one-off nginx→upstream 502 bursts from the probe itself).
EDGE_DEBOUNCE_TICKS="${ROPE_HA_EDGE_DEBOUNCE_TICKS:-2}"
PROBE_TIMEOUT_S="${ROPE_HA_PROBE_TIMEOUT_S:-3}"
FAIL_THRESHOLD="${ROPE_HA_FAIL_THRESHOLD:-2}"          # ~60s at 30s cadence
STALL_PEER_ADVANCE_S="${ROPE_HA_STALL_PEER_S:-45}"     # same hex while peer moves
STARTUP_GRACE_S="${ROPE_HA_STARTUP_GRACE_S:-300}"      # shorter than watchdog 600s
MAX_RESTARTS_PER_HOUR="${ROPE_HA_MAX_RESTARTS_PER_HOUR:-8}"
# When attesters are healthy and BLUE only fails the loopback RPC probe (not
# block_stall), defer restart so nginx can drain reads/WSS to peers instead of
# bouncing the sealer every ~90s. Force restart after PEER_DEFER_MAX_SECS.
PEER_DEFER_ENABLED="${ROPE_HA_PEER_DEFER_RESTART:-1}"
PEER_DEFER_FAIL_THRESHOLD="${ROPE_HA_PEER_DEFER_FAIL_THRESHOLD:-20}"
PEER_DEFER_MAX_SECS="${ROPE_HA_PEER_DEFER_MAX_SECS:-900}"
DUMP_BEFORE_RESTART="${ROPE_HA_DUMP_BEFORE_RESTART:-1}"
WATCHDOG_FORENSICS="${ROPE_NODE_WATCHDOG:-/opt/datachain-rope/scripts/rope-node-watchdog.sh}"
# Autonomously re-inject attester-only mempool txs into the BLUE sealer.
GHOST_RECLAIM_ENABLED="${ROPE_HA_GHOST_RECLAIM:-1}"
GHOST_RECLAIM_MAX_PER_TICK="${ROPE_HA_GHOST_RECLAIM_MAX:-32}"
GHOST_RECLAIM_TIMEOUT_S="${ROPE_HA_GHOST_RECLAIM_TIMEOUT_S:-8}"

# External-peer edge probes (spec: docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md).
# The cerber-edge-ingest.service receives signed probes from cerber-dcswap /
# cerber-tanastok / cerber-alteros and appends them here. This script tails
# the NDJSON on every tick, aggregates per-peer over a rolling window, and
# folds the result into fleet-status.edge.external_probes so CERBER peers can
# AND writer.status with the multi-vantage-point view of the public edge.
EDGE_EXTERNAL_NDJSON="${ROPE_HA_EDGE_EXTERNAL_FILE:-/var/lib/datachain-rope/fleet/external-probes.ndjson}"
EDGE_EXTERNAL_WINDOW_SECS="${ROPE_HA_EDGE_EXTERNAL_WINDOW_SECS:-900}"
EDGE_EXTERNAL_FAIL_RATIO_THRESHOLD="${ROPE_HA_EDGE_EXTERNAL_FAIL_RATIO:-0.10}"
EDGE_EXTERNAL_MIN_PEERS="${ROPE_HA_EDGE_EXTERNAL_MIN_PEERS:-2}"
EDGE_EXTERNAL_SUSTAIN_SECS="${ROPE_HA_EDGE_EXTERNAL_SUSTAIN_SECS:-180}"
EDGE_EXTERNAL_STALE_SECS="${ROPE_HA_EDGE_EXTERNAL_STALE_SECS:-3600}"
EDGE_EXTERNAL_MAX_LINES="${ROPE_HA_EDGE_EXTERNAL_MAX_LINES:-4096}"

# Non-sealer (attester) nodes: compare local tip to canonical writer via HTTPS;
# never run ghost reclaim or writer block_stall restart semantics locally.
FLEET_NODE_ROLE="${ROPE_FLEET_NODE_ROLE:-writer}"
FLEET_NODE_ID="${ROPE_FLEET_NODE_ID:-blue}"
WRITER_RPC_URL="${ROPE_WRITER_RPC:-https://erpc.datachain.network}"
SYNC_LAG_MAX_BLOCKS="${ROPE_HA_SYNC_LAG_BLOCKS:-512}"
SYNC_LAG_RESYNC_COOLDOWN_S="${ROPE_HA_SYNC_RESYNC_COOLDOWN_S:-3600}"
RESYNC_REQUEST_FILE="${ROPE_HA_RESYNC_REQUEST_FILE:-/var/lib/datachain-rope/fleet/resync-requested}"
FLEET_PUBLISH_STATUS="${ROPE_FLEET_PUBLISH_STATUS:-1}"

mkdir -p "$STATE_DIR" "$(dirname "$STATUS_FILE")" "$(dirname "$LOG")" 2>/dev/null || true
touch "$LOG" 2>/dev/null || true
chmod 755 "$STATE_DIR" "$(dirname "$STATUS_FILE")" 2>/dev/null || true

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }
now_epoch() { date -u +%s; }

rpc_call() {
  local url="$1" method="${2:-eth_blockNumber}"
  curl -sS --max-time "$PROBE_TIMEOUT_S" -X POST "$url" \
    -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"${method}\",\"params\":[]}" 2>/dev/null || true
}

parse_hex_result() {
  python3 -c 'import json,sys
try:
  j=json.load(sys.stdin); r=j.get("result")
  print(r if isinstance(r,str) else "")
except Exception:
  print("")' 2>/dev/null || echo ""
}

parse_probe_tip_hex() {
  python3 -c 'import json,sys
try:
  j=json.load(sys.stdin); h=j.get("block_hex")
  print(h if isinstance(h,str) else "")
except Exception:
  print("")' 2>/dev/null || echo ""
}

blue_probe_call() {
  if [[ -n "${ROPE_BLUE_PROBE_URL:-}" ]]; then
    curl -sS --max-time "$PROBE_TIMEOUT_S" "${ROPE_BLUE_PROBE_URL}" 2>/dev/null || true
  else
    rpc_call "$BLUE_RPC" eth_blockNumber
  fi
}

blue_probe_hex() {
  local body="$1"
  if [[ -n "${ROPE_BLUE_PROBE_URL:-}" ]]; then
    printf '%s' "$body" | parse_probe_tip_hex
  else
    printf '%s' "$body" | parse_hex_result
  fi
}

# CERBER (and humans) treat this as the SLA: if writer stays non-healthy
# longer than SELF_HEAL_DEADLINE_S after first detection, escalate - Rope
# self-heal did not close the window. Same deadline applies to public-edge
# degradation while writer.local looks fine (nginx/DNS settle gap).
SELF_HEAL_DEADLINE_S="${ROPE_HA_SELF_HEAL_DEADLINE_S:-900}"
# First-principles writer-recovery budget for SWAP CLIENTS (DCSwap 2026-08-14
# §4.2). Independent of CERBER deadline_secs (900s): a typical BLUE heal is
# detect (~60s) + restart (~90s) + STARTUP_GRACE_S (300s) ~ 7 min. Clients
# pad router deadlines from estimated_recovery_secs, not from escalate_to_cerber.
WRITER_RESTART_TYPICAL_S="${ROPE_HA_WRITER_RESTART_TYPICAL_S:-420}"
CLIENT_PAD_MIN_S="${ROPE_HA_CLIENT_PAD_MIN_S:-60}"
CLIENT_PAD_MAX_S="${ROPE_HA_CLIENT_PAD_MAX_S:-300}"
PUBLIC_READ_RPC_URL="${ROPE_HA_PUBLIC_READ_RPC_URL:-https://erpc.datachain.network/v1/read}"

# Filled by probe_public_edge before each write_status.
EDGE_SAMPLE_OK=0
EDGE_SAMPLE_FAIL=0
EDGE_STATUS="unknown"
EDGE_RESOLVED_A=""

load_state() {
  # shellcheck disable=SC1090
  if [[ -f "$STATE_FILE" ]]; then
    # state format: KEY=VALUE lines
    # fail_count, last_blue_hex, last_blue_hex_at, restart_epochs,
    # unhealthy_since, edge_unhealthy_since
    # shellcheck disable=SC1091
    source "$STATE_FILE" 2>/dev/null || true
  fi
  FAIL_COUNT="${FAIL_COUNT:-0}"
  LAST_BLUE_HEX="${LAST_BLUE_HEX:-}"
  LAST_BLUE_HEX_AT="${LAST_BLUE_HEX_AT:-0}"
  RESTART_EPOCHS="${RESTART_EPOCHS:-}"
  UNHEALTHY_SINCE="${UNHEALTHY_SINCE:-0}"
  EDGE_UNHEALTHY_SINCE="${EDGE_UNHEALTHY_SINCE:-0}"
  EDGE_BAD_STREAK="${EDGE_BAD_STREAK:-0}"
  EDGE_EXTERNAL_DEGRADED_SINCE="${EDGE_EXTERNAL_DEGRADED_SINCE:-0}"
  GHOST_RECLAIMED_TOTAL="${GHOST_RECLAIMED_TOTAL:-0}"
  GHOST_LAST_RECLAIM_AT="${GHOST_LAST_RECLAIM_AT:-0}"
  GHOST_LAST_RECLAIM_COUNT="${GHOST_LAST_RECLAIM_COUNT:-0}"
  GHOST_LAST_RECLAIM_HASHES="${GHOST_LAST_RECLAIM_HASHES:-}"
  GHOST_LAST_SCAN_GHOSTS="${GHOST_LAST_SCAN_GHOSTS:-0}"
  GHOST_LAST_SCAN_ERROR="${GHOST_LAST_SCAN_ERROR:-}"
  LAST_RESYNC_REQUEST_AT="${LAST_RESYNC_REQUEST_AT:-0}"
}

save_state() {
  cat >"$STATE_FILE" <<EOF
FAIL_COUNT=${FAIL_COUNT}
LAST_BLUE_HEX=${LAST_BLUE_HEX}
LAST_BLUE_HEX_AT=${LAST_BLUE_HEX_AT}
RESTART_EPOCHS=${RESTART_EPOCHS}
UNHEALTHY_SINCE=${UNHEALTHY_SINCE}
EDGE_UNHEALTHY_SINCE=${EDGE_UNHEALTHY_SINCE}
EDGE_BAD_STREAK=${EDGE_BAD_STREAK}
EDGE_EXTERNAL_DEGRADED_SINCE=${EDGE_EXTERNAL_DEGRADED_SINCE}
GHOST_RECLAIMED_TOTAL=${GHOST_RECLAIMED_TOTAL}
GHOST_LAST_RECLAIM_AT=${GHOST_LAST_RECLAIM_AT}
GHOST_LAST_RECLAIM_COUNT=${GHOST_LAST_RECLAIM_COUNT}
GHOST_LAST_RECLAIM_HASHES=${GHOST_LAST_RECLAIM_HASHES}
GHOST_LAST_SCAN_GHOSTS=${GHOST_LAST_SCAN_GHOSTS}
GHOST_LAST_SCAN_ERROR=${GHOST_LAST_SCAN_ERROR}
LAST_RESYNC_REQUEST_AT=${LAST_RESYNC_REQUEST_AT}
EOF
}

hex_block_diff() {
  local high="$1" low="$2"
  python3 -c "print(max(0, int('$high', 16) - int('$low', 16)))" 2>/dev/null || echo 0
}

trigger_sync_resync_if_needed() {
  local lag="$1"
  local now last
  now=$(now_epoch)
  last="${LAST_RESYNC_REQUEST_AT:-0}"
  if [[ "$lag" -le "$SYNC_LAG_MAX_BLOCKS" ]]; then
    return 0
  fi
  if [[ "$last" =~ ^[0-9]+$ ]] && [[ "$last" -gt 0 ]] && [[ $((now - last)) -lt "$SYNC_LAG_RESYNC_COOLDOWN_S" ]]; then
    return 0
  fi
  mkdir -p "$(dirname "$RESYNC_REQUEST_FILE")" 2>/dev/null || true
  echo "$(ts) lag_blocks=${lag} writer=${WRITER_RPC_URL}" >"$RESYNC_REQUEST_FILE"
  LAST_RESYNC_REQUEST_AT=$now
  save_state
  echo "$(ts) attester resync requested lag=${lag} file=${RESYNC_REQUEST_FILE}" >>"$LOG"
}

# Scan attester txpools; inject any hash missing from BLUE into the sealer.
# Idempotent: "already known" / already-pending on BLUE counts as success.
reclaim_ghost_txs() {
  if [[ "${GHOST_RECLAIM_ENABLED}" != "1" ]]; then
    return 0
  fi
  local out rc
  set +e
  out=$(
    ROPE_HA_BLUE_RPC="$BLUE_RPC" \
    ROPE_HA_PEER_RPCS="$PEER_RPCS" \
    ROPE_HA_GHOST_MAX="$GHOST_RECLAIM_MAX_PER_TICK" \
    ROPE_HA_GHOST_TIMEOUT="$GHOST_RECLAIM_TIMEOUT_S" \
    python3 - <<'PY'
import json, os, sys, urllib.request, urllib.error

blue = os.environ["ROPE_HA_BLUE_RPC"].rstrip("/")
peers = [u.rstrip("/") for u in os.environ.get("ROPE_HA_PEER_RPCS", "").split() if u]
max_n = int(os.environ.get("ROPE_HA_GHOST_MAX") or "32")
timeout = float(os.environ.get("ROPE_HA_GHOST_TIMEOUT") or "8")

def rpc(url, method, params=None, t=timeout):
    req = urllib.request.Request(
        url,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params or []}).encode(),
        headers={"content-type": "application/json", "user-agent": "datachain-erpc-fleet-ha/ghost-reclaim"},
        method="POST",
    )
    with urllib.request.urlopen(req, timeout=t) as r:
        return json.load(r)

def pool_hashes(url):
    try:
        j = rpc(url, "txpool_content", [])
    except Exception as e:
        return [], f"txpool_content:{e}"
    res = j.get("result") or {}
    out = []
    for bucket in ("pending", "queued"):
        by_addr = res.get(bucket) or {}
        if not isinstance(by_addr, dict):
            continue
        for _addr, nonces in by_addr.items():
            if not isinstance(nonces, dict):
                continue
            for _n, tx in nonces.items():
                if isinstance(tx, dict) and tx.get("hash"):
                    out.append(str(tx["hash"]).lower())
    # dedupe preserve order
    seen, uniq = set(), []
    for h in out:
        if h not in seen:
            seen.add(h)
            uniq.append(h)
    return uniq, None

def blue_knows(h):
    try:
        t = rpc(blue, "eth_getTransactionByHash", [h]).get("result")
        if t:
            return True
        r = rpc(blue, "eth_getTransactionReceipt", [h]).get("result")
        return bool(r)
    except Exception:
        return False

def get_raw(peer, h):
    last = "no_raw"
    for method in ("eth_getRawTransactionByHash", "debug_getRawTransaction"):
        try:
            raw = rpc(peer, method, [h]).get("result")
            if isinstance(raw, str) and raw.startswith("0x") and len(raw) > 10:
                return raw, None
            last = f"{method}:empty"
        except Exception as e:
            last = f"{method}:{e}"
    return None, last

report = {
    "enabled": True,
    "ghosts_found": 0,
    "reclaimed": 0,
    "skipped_known": 0,
    "errors": [],
    "hashes": [],
    "by_peer": {},
}

for peer in peers:
    hashes, err = pool_hashes(peer)
    if err:
        report["errors"].append(f"{peer}:{err}")
        continue
    peer_ghosts = []
    for h in hashes:
        if blue_knows(h):
            report["skipped_known"] += 1
            continue
        peer_ghosts.append(h)
    report["by_peer"][peer] = len(peer_ghosts)
    for h in peer_ghosts:
        if report["reclaimed"] >= max_n:
            report["errors"].append("max_per_tick_reached")
            break
        report["ghosts_found"] += 1
        raw, rerr = get_raw(peer, h)
        if not raw:
            report["errors"].append(f"{h}:raw_from_{peer}:{rerr}")
            continue
        try:
            sent = rpc(blue, "eth_sendRawTransaction", [raw], t=timeout + 4)
        except Exception as e:
            report["errors"].append(f"{h}:send:{e}")
            continue
        if sent.get("error"):
            msg = str(sent["error"].get("message") or sent["error"])
            # already known / nonce too low after concurrent mine = ok
            if any(x in msg.lower() for x in ("already known", "nonce too low", "known transaction")):
                report["reclaimed"] += 1
                report["hashes"].append(h)
            else:
                report["errors"].append(f"{h}:send_err:{msg}")
            continue
        got = (sent.get("result") or "").lower()
        if got == h or blue_knows(h):
            report["reclaimed"] += 1
            report["hashes"].append(h)
        else:
            report["errors"].append(f"{h}:hash_mismatch:{got}")

print(json.dumps(report))
PY
  )
  rc=$?
  set -e
  if [[ "$rc" -ne 0 || -z "$out" ]]; then
    GHOST_LAST_SCAN_ERROR="reclaim_script_failed_rc=${rc}"
    echo "$(ts) ghost.reclaim ERROR ${GHOST_LAST_SCAN_ERROR}" >>"$LOG"
    return 0
  fi
  # Parse summary into shell state (best-effort).
  local tmp_json
  tmp_json="$(mktemp)"
  printf '%s' "$out" >"$tmp_json"
  GHOST_LAST_SCAN_GHOSTS="$(python3 -c 'import json,sys; print(int(json.load(open(sys.argv[1])).get("ghosts_found") or 0))' "$tmp_json" 2>/dev/null || echo 0)"
  _GHOST_TICK_RECLAIMED="$(python3 -c 'import json,sys; print(int(json.load(open(sys.argv[1])).get("reclaimed") or 0))' "$tmp_json" 2>/dev/null || echo 0)"
  GHOST_LAST_RECLAIM_HASHES="$(python3 -c 'import json,sys; h=",".join(json.load(open(sys.argv[1])).get("hashes") or [])[:500]; print("".join(c if c.isalnum() or c in "._-:," else "_" for c in h))' "$tmp_json" 2>/dev/null || true)"
  GHOST_LAST_SCAN_ERROR="$(python3 -c 'import json,sys; e=";".join(json.load(open(sys.argv[1])).get("errors") or [])[:400]; s="".join(c if c.isalnum() or c in "._-:," else "_" for c in e); print(s or "none")' "$tmp_json" 2>/dev/null || echo parse_failed)"
  rm -f "$tmp_json"
  _GHOST_TICK_RECLAIMED="${_GHOST_TICK_RECLAIMED:-0}"
  if [[ "${_GHOST_TICK_RECLAIMED}" =~ ^[0-9]+$ ]] && [[ "${_GHOST_TICK_RECLAIMED}" -gt 0 ]]; then
    GHOST_RECLAIMED_TOTAL=$(( ${GHOST_RECLAIMED_TOTAL:-0} + _GHOST_TICK_RECLAIMED ))
    GHOST_LAST_RECLAIM_AT=$(now_epoch)
    GHOST_LAST_RECLAIM_COUNT="${_GHOST_TICK_RECLAIMED}"
    echo "$(ts) ghost.reclaim RECLAIMED count=${_GHOST_TICK_RECLAIMED} total=${GHOST_RECLAIMED_TOTAL} hashes=${GHOST_LAST_RECLAIM_HASHES}" >>"$LOG"
  elif [[ "${GHOST_LAST_SCAN_GHOSTS:-0}" =~ ^[0-9]+$ ]] && [[ "${GHOST_LAST_SCAN_GHOSTS}" -gt 0 ]]; then
    echo "$(ts) ghost.reclaim FOUND ghosts=${GHOST_LAST_SCAN_GHOSTS} reclaimed=0 err=${GHOST_LAST_SCAN_ERROR}" >>"$LOG"
  fi
  save_state
}

# Sample the public HTTPS vhost the way MetaMask / DCSwap bots see it.
# Loopback writer health alone missed the 2026-07-28 3/10×502 window.
#
# Important: hit nginx via --resolve …:127.0.0.1 (same TLS server_name +
# upstream path) rather than NAT-hairpin to the public A. Hairpin + a burst
# of N parallel curls from BLUE's own egress IP shares the rpc limit_req
# bucket with real traffic and was producing false "degraded" (3/10) while
# external clients still saw 10/10. DNS A is still published in resolved_a.
probe_public_edge() {
  local ok=0 fail=0 resolved="" tmpdir i
  local edge_timeout="${ROPE_HA_EDGE_PROBE_TIMEOUT_S:-2}"
  local edge_ip="${ROPE_HA_EDGE_PROBE_IP:-127.0.0.1}"
  local raw_status tick_bad=0
  EDGE_SAMPLE_OK=0
  EDGE_SAMPLE_FAIL=0
  EDGE_STATUS="unknown"
  EDGE_RESOLVED_A=""
  tmpdir=$(mktemp -d /tmp/erpc-ha-edge.XXXXXX 2>/dev/null || echo "/tmp/erpc-ha-edge.$$")
  mkdir -p "$tmpdir"
  # Sequential - parallel bursts were inducing nginx→upstream 502s (172.18.0.1).
  for i in $(seq 1 "$EDGE_SAMPLE_N"); do
    http=$(curl -sS --max-time "$edge_timeout" \
      --resolve "${PUBLIC_EDGE_HOST}:443:${edge_ip}" \
      -o "$tmpdir/b$i" -w '%{http_code}' \
      -X POST "$PUBLIC_EDGE_URL" \
      -H 'content-type: application/json' \
      -H 'user-agent: datachain-erpc-fleet-ha/edge-probe' \
      -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' 2>/dev/null || echo "000")
    if [[ "$http" == "200" ]] && grep -q '"result"' "$tmpdir/b$i" 2>/dev/null; then
      ok=$((ok + 1))
    else
      fail=$((fail + 1))
    fi
  done
  rm -rf "$tmpdir" 2>/dev/null || true
  EDGE_SAMPLE_OK=$ok
  EDGE_SAMPLE_FAIL=$fail
  if [[ "$ok" -eq "$EDGE_SAMPLE_N" ]]; then
    raw_status="healthy"
  elif [[ "$ok" -eq 0 ]]; then
    raw_status="down"
    tick_bad=1
  else
    if python3 -c "import sys; ok=int(sys.argv[1]); n=int(sys.argv[2]); r=float(sys.argv[3]);
sys.exit(0 if (n-ok)/float(n) > r else 1)" "$ok" "$EDGE_SAMPLE_N" "$EDGE_FAIL_RATIO"; then
      raw_status="degraded"
      tick_bad=1
    else
      raw_status="healthy"
    fi
  fi
  if [[ "$tick_bad" -eq 1 ]]; then
    EDGE_BAD_STREAK=$(( ${EDGE_BAD_STREAK:-0} + 1 ))
  else
    EDGE_BAD_STREAK=0
  fi
  # Publish raw samples always; only flip edge.status after debounce streak
  # (except down with 0/N - publish immediately).
  if [[ "$raw_status" == "down" ]]; then
    EDGE_STATUS="down"
  elif [[ "$tick_bad" -eq 1 && "$EDGE_BAD_STREAK" -ge "$EDGE_DEBOUNCE_TICKS" ]]; then
    EDGE_STATUS="degraded"
  else
    EDGE_STATUS="healthy"
  fi
  if command -v dig >/dev/null 2>&1; then
    resolved=$(dig +short "$PUBLIC_EDGE_HOST" A 2>/dev/null | grep -E '^[0-9.]+$' | tr '\n' ' ' | sed 's/[[:space:]]*$//')
  fi
  if [[ -z "$resolved" ]]; then
    resolved=$(getent ahostsv4 "$PUBLIC_EDGE_HOST" 2>/dev/null | awk '{print $1}' | sort -u | tr '\n' ' ' | sed 's/[[:space:]]*$//')
  fi
  EDGE_RESOLVED_A="$resolved"

  if [[ "$EDGE_STATUS" == "healthy" ]]; then
    EDGE_UNHEALTHY_SINCE=0
  elif [[ -z "${EDGE_UNHEALTHY_SINCE:-}" || "${EDGE_UNHEALTHY_SINCE}" == "0" ]]; then
    EDGE_UNHEALTHY_SINCE=$(now_epoch)
  fi
}

# Tail the cerber-edge-ingest NDJSON, aggregate per peer over a rolling
# window, and print the fleet-status.edge.external_probes JSON block.
# Spec: docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md §3.
#
# Prints a JSON object (never a partial one) on stdout so callers can inline
# it into fleet-status.json. Missing file, unreadable file, or zero-line file
# still prints a well-formed object with peers=[] and summary counters at 0
# so downstream consumers never see undefined-shape drift.
read_external_probes() {
  ROPE_HA_EXT_FILE="$EDGE_EXTERNAL_NDJSON" \
  ROPE_HA_EXT_WINDOW="$EDGE_EXTERNAL_WINDOW_SECS" \
  ROPE_HA_EXT_RATIO="$EDGE_EXTERNAL_FAIL_RATIO_THRESHOLD" \
  ROPE_HA_EXT_MIN_PEERS="$EDGE_EXTERNAL_MIN_PEERS" \
  ROPE_HA_EXT_SUSTAIN="$EDGE_EXTERNAL_SUSTAIN_SECS" \
  ROPE_HA_EXT_STALE="$EDGE_EXTERNAL_STALE_SECS" \
  ROPE_HA_EXT_MAX_LINES="$EDGE_EXTERNAL_MAX_LINES" \
  ROPE_HA_EXT_DEGRADED_SINCE="${EDGE_EXTERNAL_DEGRADED_SINCE:-0}" \
  python3 - <<'PY' 2>/dev/null || printf '%s' '{"enabled":false,"peers":[],"reason":"aggregator_failed"}'
import collections, json, math, os, sys, time

path = os.environ.get("ROPE_HA_EXT_FILE") or ""
window = int(os.environ.get("ROPE_HA_EXT_WINDOW") or "900")
ratio_threshold = float(os.environ.get("ROPE_HA_EXT_RATIO") or "0.1")
min_peers = int(os.environ.get("ROPE_HA_EXT_MIN_PEERS") or "2")
sustain = int(os.environ.get("ROPE_HA_EXT_SUSTAIN") or "180")
stale_after = int(os.environ.get("ROPE_HA_EXT_STALE") or "3600")
max_lines = int(os.environ.get("ROPE_HA_EXT_MAX_LINES") or "4096")
prior_degraded_since = int(os.environ.get("ROPE_HA_EXT_DEGRADED_SINCE") or "0")
now = int(time.time())
window_low = now - window

def emit(doc, degraded_since=0):
    doc["degraded_since"] = degraded_since if degraded_since > 0 else None
    doc["degraded_for_secs"] = max(now - degraded_since, 0) if degraded_since > 0 else 0
    doc["sustain_secs"] = sustain
    doc["fail_ratio_threshold"] = ratio_threshold
    doc["min_peers_for_escalation"] = min_peers
    doc["window_secs"] = window
    doc["generated_at"] = now
    # Escalate if we have >= min_peers peers currently degraded AND we've been
    # in that state continuously for >= sustain_secs.
    degraded_peer_ids = [p["peer_id"] for p in doc.get("peers", []) if p.get("status") == "degraded"]
    doc["degraded_peer_count"] = len(degraded_peer_ids)
    doc["degraded_peer_ids"] = degraded_peer_ids
    should_escalate = (
        len(degraded_peer_ids) >= min_peers
        and degraded_since > 0
        and (now - degraded_since) >= sustain
    )
    doc["escalate"] = should_escalate
    print(json.dumps(doc))
    sys.exit(0)

if not path or not os.path.isfile(path):
    emit({"enabled": False, "reason": "no_ingest_file", "path": path or None, "peers": []})

# Tail the last max_lines physical lines cheaply.
try:
    with open(path, "rb") as fh:
        fh.seek(0, 2)
        size = fh.tell()
        chunk = min(size, 1024 * 1024)  # 1 MB tail is more than enough for 4096 lines
        fh.seek(max(0, size - chunk), 0)
        tail = fh.read().decode("utf-8", errors="replace")
except Exception as exc:
    emit({"enabled": False, "reason": f"read_error:{type(exc).__name__}", "peers": []})

raw_lines = [ln for ln in tail.splitlines() if ln.strip()]
raw_lines = raw_lines[-max_lines:]

by_peer = collections.OrderedDict()
oldest_report = None
newest_report = None
total_reports = 0
malformed = 0
for line in raw_lines:
    try:
        entry = json.loads(line)
    except Exception:
        malformed += 1
        continue
    body = entry.get("body") or {}
    peer_id = body.get("peer_id") or entry.get("peer_id")
    win_end = body.get("window_end")
    if not peer_id or not isinstance(win_end, (int, float)):
        malformed += 1
        continue
    win_end = int(win_end)
    if win_end < window_low:
        # Older than aggregation window - ignore for the rolling counters.
        continue
    total_reports += 1
    if oldest_report is None or win_end < oldest_report:
        oldest_report = win_end
    if newest_report is None or win_end > newest_report:
        newest_report = win_end
    sample_n = int(body.get("sample_n") or 0)
    sample_ok = int(body.get("sample_ok") or 0)
    sample_fail = int(body.get("sample_fail") or (sample_n - sample_ok))
    reasons = body.get("reasons") or {}
    entry_region = body.get("peer_source_region") or entry.get("peer_source_region")
    resolver_ip = body.get("resolver_ip") or entry.get("resolver_ip")
    target_url = body.get("target_url") or entry.get("target_url")
    slot = by_peer.setdefault(peer_id, {
        "peer_id": peer_id,
        "region": entry_region,
        "resolver_ip": resolver_ip,
        "target_url": target_url,
        "sample_n_agg": 0,
        "sample_ok_agg": 0,
        "sample_fail_agg": 0,
        "reasons_agg": collections.Counter(),
        "reports": 0,
        "last_report_at": 0,
        "first_report_at": win_end,
        "last_reason": None,
    })
    slot["sample_n_agg"] += sample_n
    slot["sample_ok_agg"] += sample_ok
    slot["sample_fail_agg"] += sample_fail
    slot["reports"] += 1
    if win_end > slot["last_report_at"]:
        slot["last_report_at"] = win_end
        slot["region"] = entry_region or slot["region"]
        slot["resolver_ip"] = resolver_ip or slot["resolver_ip"]
        slot["target_url"] = target_url or slot["target_url"]
        if reasons:
            worst = None
            worst_count = 0
            for reason_key, reason_count in reasons.items():
                try:
                    count_int = int(reason_count)
                except Exception:
                    continue
                if count_int > worst_count:
                    worst_count = count_int
                    worst = reason_key
            slot["last_reason"] = worst
    if isinstance(reasons, dict):
        for reason_key, reason_count in reasons.items():
            try:
                slot["reasons_agg"][reason_key] += int(reason_count)
            except Exception:
                pass

peers_out = []
degraded_now = 0
for peer_id, slot in by_peer.items():
    n = slot["sample_n_agg"]
    fail = slot["sample_fail_agg"]
    ratio = (fail / n) if n > 0 else 0.0
    age = max(now - slot["last_report_at"], 0)
    if age > stale_after:
        status = "stale"
    elif n <= 0:
        status = "unknown"
    elif ratio > ratio_threshold:
        status = "degraded"
        degraded_now += 1
    else:
        status = "ok"
    peers_out.append({
        "peer_id": peer_id,
        "region": slot["region"],
        "resolver_ip": slot["resolver_ip"],
        "target_url": slot["target_url"],
        "sample_n": n,
        "sample_ok": slot["sample_ok_agg"],
        "sample_fail": fail,
        "fail_ratio": round(ratio, 4),
        "reports": slot["reports"],
        "last_report_at": slot["last_report_at"],
        "last_reason": slot["last_reason"],
        "age_secs": age,
        "status": status,
        "reasons": dict(slot["reasons_agg"]),
    })

# Track continuous-degraded epoch for the sustain check.
degraded_since = prior_degraded_since
if degraded_now >= min_peers:
    if degraded_since <= 0:
        degraded_since = now
else:
    degraded_since = 0

doc = {
    "enabled": True,
    "peers": peers_out,
    "peer_count": len(peers_out),
    "reports_in_window": total_reports,
    "malformed_lines": malformed,
    "oldest_report_at": oldest_report,
    "newest_report_at": newest_report,
    "ingest_path": path,
}
emit(doc, degraded_since=degraded_since)
PY
}

in_startup_grace() {
  local start_ts_raw age start_epoch
  start_ts_raw=$(systemctl show -p ExecMainStartTimestamp --value datachain-rope.service 2>/dev/null || true)
  [[ -z "$start_ts_raw" || "$start_ts_raw" == "n/a" ]] && return 1
  start_epoch=$(date -d "$start_ts_raw" +%s 2>/dev/null || echo 0)
  [[ "$start_epoch" -le 0 ]] && return 1
  age=$(( $(now_epoch) - start_epoch ))
  [[ "$age" -lt "$STARTUP_GRACE_S" ]]
}

restarts_in_last_hour() {
  local cutoff now e count=0
  now=$(now_epoch)
  cutoff=$((now - 3600))
  IFS=',' read -r -a arr <<<"${RESTART_EPOCHS}"
  for e in "${arr[@]:-}"; do
    [[ -z "$e" ]] && continue
    if [[ "$e" =~ ^[0-9]+$ ]] && [[ "$e" -ge "$cutoff" ]]; then
      count=$((count + 1))
    fi
  done
  echo "$count"
}

record_restart() {
  local now pruned="" e
  now=$(now_epoch)
  RESTART_EPOCHS="${RESTART_EPOCHS:+${RESTART_EPOCHS},}${now}"
  # prune >1h
  IFS=',' read -r -a arr <<<"${RESTART_EPOCHS}"
  for e in "${arr[@]}"; do
    [[ -z "$e" ]] && continue
    if [[ "$e" =~ ^[0-9]+$ ]] && [[ "$e" -ge $((now - 3600)) ]]; then
      pruned="${pruned:+${pruned},}${e}"
    fi
  done
  RESTART_EPOCHS="$pruned"
}

write_status() {
  local writer_status="$1" reason="$2" blue_hex="$3" peer_hex="$4" unhealthy_csv="$5"
  local rh now unsince age_unhealthy edge_unsince age_edge escalate=0
  local ext_probes_json ext_degraded_since ext_escalate
  # Always measure the public surface peers actually hit (additive edge object).
  probe_public_edge
  # Aggregate external-peer probes (cerber-dcswap / -tanastok / -alteros) into
  # the multi-vantage-point view (spec §3, docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md).
  ext_probes_json=$(read_external_probes)
  if [[ -z "$ext_probes_json" ]]; then
    ext_probes_json='{"enabled":false,"peers":[],"reason":"aggregator_empty"}'
  fi
  ext_degraded_since=$(printf '%s' "$ext_probes_json" | python3 -c 'import json,sys
try:
    v=json.loads(sys.stdin.read()).get("degraded_since")
    print(int(v) if v else 0)
except Exception:
    print(0)' 2>/dev/null || echo 0)
  EDGE_EXTERNAL_DEGRADED_SINCE="${ext_degraded_since:-0}"
  ext_escalate=$(printf '%s' "$ext_probes_json" | python3 -c 'import json,sys
try:
    print("1" if json.loads(sys.stdin.read()).get("escalate") else "0")
except Exception:
    print("0")' 2>/dev/null || echo 0)
  rh=$(restarts_in_last_hour)
  now=$(now_epoch)
  local service_age=0 start_ts_raw start_epoch
  start_ts_raw=$(systemctl show -p ExecMainStartTimestamp --value datachain-rope.service 2>/dev/null || true)
  if [[ -n "$start_ts_raw" && "$start_ts_raw" != "n/a" ]]; then
    start_epoch=$(date -d "$start_ts_raw" +%s 2>/dev/null || echo 0)
    if [[ "$start_epoch" =~ ^[0-9]+$ ]] && [[ "$start_epoch" -gt 0 ]]; then
      service_age=$((now - start_epoch))
      [[ "$service_age" -lt 0 ]] && service_age=0
    fi
  fi
  unsince="${UNHEALTHY_SINCE:-0}"
  edge_unsince="${EDGE_UNHEALTHY_SINCE:-0}"
  age_unhealthy=0
  age_edge=0
  if [[ "$unsince" =~ ^[0-9]+$ ]] && [[ "$unsince" -gt 0 ]]; then
    age_unhealthy=$((now - unsince))
  fi
  if [[ "$edge_unsince" =~ ^[0-9]+$ ]] && [[ "$edge_unsince" -gt 0 ]]; then
    age_edge=$((now - edge_unsince))
  fi
  # Escalate when writer non-healthy past deadline, restart cap, public edge
  # stays degraded/down past the same SLA while writer looks fine, OR the
  # multi-vantage-point view (external CERBER peers) shows sustained
  # degradation (min_peers ratio + sustain_secs — see edge.external_probes).
  if [[ "$writer_status" == "out_of_service" ]]; then
    escalate=1
  elif [[ "$writer_status" != "healthy" && "$age_unhealthy" -ge "$SELF_HEAL_DEADLINE_S" ]]; then
    escalate=1
  elif [[ "$EDGE_STATUS" != "healthy" && "$EDGE_STATUS" != "unknown" && "$age_edge" -ge "$SELF_HEAL_DEADLINE_S" ]]; then
    escalate=1
  elif [[ "$ext_escalate" == "1" ]]; then
    escalate=1
  fi
  save_state
  ROPE_HA_WSTATUS="$writer_status" \
  ROPE_HA_REASON="$reason" \
  ROPE_HA_BLUE_HEX="$blue_hex" \
  ROPE_HA_PEER_HEX="$peer_hex" \
  ROPE_HA_UNHEALTHY="$unhealthy_csv" \
  ROPE_HA_BLUE_RPC="$BLUE_RPC" \
  ROPE_HA_PEER_RPCS="$PEER_RPCS" \
  ROPE_HA_RESTARTS="$rh" \
  ROPE_HA_ISO="$(ts)" \
  ROPE_HA_UNHEALTHY_SINCE="$unsince" \
  ROPE_HA_UNHEALTHY_AGE="$age_unhealthy" \
  ROPE_HA_DEADLINE="$SELF_HEAL_DEADLINE_S" \
  ROPE_HA_ESCALATE="$escalate" \
  ROPE_HA_TYPICAL="$WRITER_RESTART_TYPICAL_S" \
  ROPE_HA_GRACE="$STARTUP_GRACE_S" \
  ROPE_HA_SVC_AGE="$service_age" \
  ROPE_HA_PAD_MIN="$CLIENT_PAD_MIN_S" \
  ROPE_HA_PAD_MAX="$CLIENT_PAD_MAX_S" \
  ROPE_HA_PUBLIC_READ="$PUBLIC_READ_RPC_URL" \
  ROPE_HA_EDGE_URL="$PUBLIC_EDGE_URL" \
  ROPE_HA_EDGE_STATUS="$EDGE_STATUS" \
  ROPE_HA_EDGE_OK="$EDGE_SAMPLE_OK" \
  ROPE_HA_EDGE_N="$EDGE_SAMPLE_N" \
  ROPE_HA_EDGE_FAIL="$EDGE_SAMPLE_FAIL" \
  ROPE_HA_EDGE_A="$EDGE_RESOLVED_A" \
  ROPE_HA_EDGE_SINCE="$edge_unsince" \
  ROPE_HA_EDGE_AGE="$age_edge" \
  ROPE_HA_EDGE_RATIO="$EDGE_FAIL_RATIO" \
  ROPE_HA_GHOST_ENABLED="$GHOST_RECLAIM_ENABLED" \
  ROPE_HA_GHOST_TOTAL="${GHOST_RECLAIMED_TOTAL:-0}" \
  ROPE_HA_GHOST_LAST_AT="${GHOST_LAST_RECLAIM_AT:-0}" \
  ROPE_HA_GHOST_LAST_COUNT="${GHOST_LAST_RECLAIM_COUNT:-0}" \
  ROPE_HA_GHOST_LAST_HASHES="${GHOST_LAST_RECLAIM_HASHES:-}" \
  ROPE_HA_GHOST_LAST_SCAN="${GHOST_LAST_SCAN_GHOSTS:-0}" \
  ROPE_HA_GHOST_LAST_ERR="${GHOST_LAST_SCAN_ERROR:-}" \
  ROPE_HA_EXT_PROBES_JSON="$ext_probes_json" \
  ROPE_HA_EXT_ESCALATE="$ext_escalate" \
  ROPE_HA_NODE_ROLE="${ROPE_HA_NODE_ROLE:-writer}" \
  ROPE_HA_NODE_ID="${ROPE_HA_NODE_ID:-blue}" \
  ROPE_HA_LOCAL_HEX="${ROPE_HA_LOCAL_HEX:-}" \
  ROPE_HA_WRITER_CANONICAL_HEX="${ROPE_HA_WRITER_CANONICAL_HEX:-}" \
  ROPE_HA_WRITER_STATUS="${ROPE_HA_WRITER_STATUS:-}" \
  ROPE_HA_SYNC_LAG="${ROPE_HA_SYNC_LAG:-0}" \
  python3 - <<'PY' >"${STATUS_FILE}.tmp"
import json, os, time
unhealthy = [x for x in os.environ.get("ROPE_HA_UNHEALTHY", "").split(",") if x]
peers = [u for u in os.environ.get("ROPE_HA_PEER_RPCS", "").split() if u]
blue_hex = os.environ.get("ROPE_HA_BLUE_HEX") or None
peer_hex = os.environ.get("ROPE_HA_PEER_HEX") or None
unsince = int(os.environ.get("ROPE_HA_UNHEALTHY_SINCE") or "0")
deadline = int(os.environ.get("ROPE_HA_DEADLINE") or "900")
age = int(os.environ.get("ROPE_HA_UNHEALTHY_AGE") or "0")
escalate = os.environ.get("ROPE_HA_ESCALATE") == "1"
edge_ok = int(os.environ.get("ROPE_HA_EDGE_OK") or "0")
edge_n = int(os.environ.get("ROPE_HA_EDGE_N") or "0")
edge_fail = int(os.environ.get("ROPE_HA_EDGE_FAIL") or "0")
edge_since = int(os.environ.get("ROPE_HA_EDGE_SINCE") or "0")
edge_age = int(os.environ.get("ROPE_HA_EDGE_AGE") or "0")
resolved = [x for x in os.environ.get("ROPE_HA_EDGE_A", "").split() if x]
try:
    ext_probes = json.loads(os.environ.get("ROPE_HA_EXT_PROBES_JSON") or '{"enabled":false,"peers":[]}')
except Exception:
    ext_probes = {"enabled": False, "peers": [], "reason": "parse_error"}
ext_escalate = os.environ.get("ROPE_HA_EXT_ESCALATE") == "1"
wstatus = os.environ.get("ROPE_HA_WSTATUS") or "unknown"
typical = int(os.environ.get("ROPE_HA_TYPICAL") or "420")
grace = int(os.environ.get("ROPE_HA_GRACE") or "300")
svc_age = int(os.environ.get("ROPE_HA_SVC_AGE") or "0")
pad_min = int(os.environ.get("ROPE_HA_PAD_MIN") or "60")
pad_max = int(os.environ.get("ROPE_HA_PAD_MAX") or "300")
now_ts = int(time.time())
remaining = 0
if wstatus in ("starting", "healing"):
    remaining = max(0, grace - svc_age)
elif wstatus != "healthy":
    remaining = max(0, typical - age)
est_at = (now_ts + remaining) if remaining > 0 else None
if remaining > 0:
    pad = min(pad_max, max(pad_min, remaining + 30))
else:
    pad = 0
public_read = os.environ.get("ROPE_HA_PUBLIC_READ") or "https://erpc.datachain.network/v1/read"
node_role = os.environ.get("ROPE_HA_NODE_ROLE") or "writer"
local_hex = os.environ.get("ROPE_HA_LOCAL_HEX") or None
writer_canonical = os.environ.get("ROPE_HA_WRITER_CANONICAL_HEX") or blue_hex
writer_surface_status = os.environ.get("ROPE_HA_WRITER_STATUS") or wstatus
sync_lag = int(os.environ.get("ROPE_HA_SYNC_LAG") or "0")
ghost_enabled = os.environ.get("ROPE_HA_GHOST_ENABLED") == "1" and node_role != "attester"
doc = {
  "schema": "datachain.erpc.fleet-status/v1",
  "generated_at": int(time.time()),
  "generated_at_iso": os.environ.get("ROPE_HA_ISO"),
  "writer": {
    "id": "blue",
    "rpc": os.environ.get("ROPE_HA_BLUE_RPC"),
    "public_edge": os.environ.get("ROPE_HA_EDGE_URL") or "https://erpc.datachain.network",
    "status": writer_surface_status if node_role == "attester" else os.environ.get("ROPE_HA_WSTATUS"),
    "block_hex": writer_canonical if node_role == "attester" else blue_hex,
  },
  "edge": {
    "public_url": os.environ.get("ROPE_HA_EDGE_URL") or "https://erpc.datachain.network",
    "status": os.environ.get("ROPE_HA_EDGE_STATUS") or "unknown",
    "sample_ok": edge_ok,
    "sample_n": edge_n,
    "sample_fail": edge_fail,
    "fail_ratio": round(edge_fail / edge_n, 3) if edge_n else None,
    "fail_ratio_threshold": float(os.environ.get("ROPE_HA_EDGE_RATIO") or "0.4"),
    "resolved_a": resolved,
    "degraded_since": edge_since if edge_since > 0 else None,
    "degraded_for_secs": edge_age if edge_since > 0 else 0,
    "external_probes": ext_probes,
    "note": "Peers must AND writer.status with edge.status - local writer "
            "healthy can coexist with public HTTPS 502s (nginx/DNS settle). "
            "edge.external_probes carries the multi-vantage-point view fed "
            "by cerber-* peers (spec: docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md).",
  },
  "readers": [{"url": u, "role": "reader"} for u in peers],
  "public_read_rpc_urls": [public_read],
  "unhealthy": unhealthy,
  "peer_sample_block_hex": peer_hex,
  "self_heal": {
    "unhealthy_since": unsince if unsince > 0 else None,
    "unhealthy_for_secs": age if unsince > 0 else 0,
    "deadline_secs": deadline,
    "escalate_to_cerber": escalate,
    "escalate_reason_external_probes": ext_escalate,
    "writer_restart_typical_secs": typical,
    "startup_grace_secs": grace,
    "writer_service_age_secs": svc_age,
    "estimated_recovery_secs": remaining,
    "estimated_recovery_at": est_at,
    "recommended_deadline_padding_secs": pad,
    "note": "CERBER pages when escalate_to_cerber is true (writer/edge "
            "non-healthy past deadline_secs, currently 900s). Swap clients "
            "MUST pad from recommended_deadline_padding_secs / "
            "estimated_recovery_at instead: writer=starting|healing still "
            "has pad>0 while escalate stays false. public_read_rpc_urls is "
            "the attester-only HTTPS reader (no BLUE) for FallbackProvider.",
  },
  "last_heal": {
    "reason": os.environ.get("ROPE_HA_REASON") or "",
    "restarts_last_hour": int(os.environ.get("ROPE_HA_RESTARTS") or "0"),
  },
  "ghost_reclaim": {
    "enabled": ghost_enabled,
    "reclaimed_total": int(os.environ.get("ROPE_HA_GHOST_TOTAL") or "0"),
    "last_reclaim_at": int(os.environ.get("ROPE_HA_GHOST_LAST_AT") or "0") or None,
    "last_reclaim_count": int(os.environ.get("ROPE_HA_GHOST_LAST_COUNT") or "0"),
    "last_reclaim_hashes": [h for h in (os.environ.get("ROPE_HA_GHOST_LAST_HASHES") or "").split(",") if h],
    "last_scan_ghosts_found": int(os.environ.get("ROPE_HA_GHOST_LAST_SCAN") or "0"),
    "last_scan_error": os.environ.get("ROPE_HA_GHOST_LAST_ERR") or None,
    "note": "Every HA tick diffs attester txpools vs BLUE; ghost raw txs "
            "are eth_sendRawTransaction'd into the sealer (2026-07-29)."
            if node_role != "attester" else
            "Disabled on attester nodes; only the active sealer reclaims ghosts.",
  },
  "notes": [
    "eth_* reads fail over via nginx rpc_read_failover when BLUE times out",
    "eth_sendRawTransaction and all rope_* stay pinned to BLUE (no dual-writer)",
    "txpool_* is primary-only so public pending matches the sealer mempool",
    "ghost_reclaim injects attester-only mempool txs into BLUE automatically",
    "sick BLUE self-heals via systemctl restart datachain-rope.service",
    "edge.* is sampled every HA tick against the public HTTPS URL (not loopback)",
    "users must never edit MetaMask RPC - wallets pull https://dcscan.io/api/v1/network/config",
    "https://erpc.datachain.network/v1/read is attester-only (GREEN/DO); never send writes there",
  ],
}
if node_role == "attester":
  doc["local_node"] = {
    "id": os.environ.get("ROPE_HA_NODE_ID") or "attester",
    "role": "attester",
    "rpc": os.environ.get("ROPE_HA_BLUE_RPC"),
    "status": os.environ.get("ROPE_HA_WSTATUS") or "unknown",
    "block_hex": local_hex,
    "sync_lag_blocks": sync_lag,
    "writer_rpc": os.environ.get("ROPE_HA_WRITER_RPC") or None,
  }
  doc["writer"]["note"] = (
    "Canonical sealer tip from ROPE_WRITER_RPC; this host is a non-sealer attester."
  )
  doc["notes"].append(
    "attester role: sync lag does not restart the local node; Reth resync is driven by the sealer replicate job"
  )
print(json.dumps(doc, indent=2))
PY
  # Sign the TEMP file first, then promote status+sig together.
  # Signing after mv left a window where R13 fetched new body + old sig
  # → false body_hash_mismatch pages (2026-07-29 CERBER R13).
  local sig_final="${STATUS_FILE%.json}.sig.json"
  local signed_final="${STATUS_FILE%.json}.signed.json"
  local sig_tmp="${STATUS_FILE}.sig.tmp"
  local signed_tmp="${STATUS_FILE}.signed.tmp"
  if sign_fleet_status_for_cerber "${STATUS_FILE}.tmp" "$sig_tmp"; then
    # Atomic-ish pair: body then envelope (ms window). Also publish a single
    # signed bundle so verifiers can avoid the two-GET race entirely.
    python3 - "${STATUS_FILE}.tmp" "$sig_tmp" "$signed_tmp" <<'PY' 2>>"$LOG" || true
import json, sys
body_path, sig_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]
body = json.load(open(body_path))
env = json.load(open(sig_path))
with open(out_path, "w") as f:
    json.dump(
        {"schema": "datachain.erpc.fleet-status.signed/v1", "body": body, "envelope": env},
        f,
        indent=2,
    )
    f.write("\n")
PY
    mv -f "${STATUS_FILE}.tmp" "$STATUS_FILE"
    mv -f "$sig_tmp" "$sig_final"
    if [[ -f "$signed_tmp" ]]; then
      mv -f "$signed_tmp" "$signed_final"
      chmod 644 "$signed_final" 2>/dev/null || true
    fi
  else
    # Do not promote an unsigned body over a previously signed pair.
    echo "$(ts) cerber.sign FAIL - keeping previous fleet-status" >>"$LOG"
    rm -f "${STATUS_FILE}.tmp" "$sig_tmp" "$signed_tmp"
    return 0
  fi
  chmod 644 "$STATUS_FILE" "$sig_final" 2>/dev/null || true
}

# Args: <body.json> [out.sig.json]. Returns 0 only when the sig file was written.
sign_fleet_status_for_cerber() {
  local status_file="$1"
  local sig_file="${2:-${status_file%.json}.sig.json}"
  local sign_bin="${ROPE_CERBER_SIGN_BIN:-/opt/datachain-rope/cerber/bin/cerber-sign.mjs}"
  local key="${CERBER_IDENTITY_KEY:-/var/lib/datachain-rope/cerber/identity.pem}"
  if [[ ! -f "$sign_bin" ]]; then
    # Local workspace fallback when not yet installed under /opt
    if [[ -f "$(dirname "$0")/../cerber/bin/cerber-sign.mjs" ]]; then
      sign_bin="$(cd "$(dirname "$0")/../cerber/bin" && pwd)/cerber-sign.mjs"
    else
      echo "$(ts) cerber.sign SKIP missing ${sign_bin}" >>"$LOG"
      return 1
    fi
  fi
  if ! command -v node >/dev/null 2>&1; then
    echo "$(ts) cerber.sign SKIP node_not_installed" >>"$LOG"
    return 1
  fi
  if ! CERBER_IDENTITY_KEY="$key" CERBER_PEER_ID="${CERBER_PEER_ID:-cerber-rope}" \
    node "$sign_bin" --kind fleet_status --file "$status_file" --out "$sig_file" \
    >>"$LOG" 2>&1; then
    echo "$(ts) cerber.sign FAIL" >>"$LOG"
    return 1
  fi
  chmod 644 "$sig_file" 2>/dev/null || true
  return 0
}

capture_dump() {
  local pid fdir
  pid=$(systemctl show -p MainPID --value datachain-rope.service 2>/dev/null || echo 0)
  [[ -z "$pid" || "$pid" == "0" ]] && return 0
  fdir="/home/ubuntu/rope-node-hang-ha-$(date -u +%Y-%m-%dT%H%M%SZ)"
  mkdir -p "$fdir" 2>/dev/null || true
  {
    echo "captured_at=$(ts)"
    echo "pid=$pid"
    echo "source=erpc-fleet-ha"
  } >"$fdir/meta.txt" 2>/dev/null || true
  cp "/proc/$pid/status" "$fdir/status.txt" 2>/dev/null || true
  if command -v eu-stack >/dev/null 2>&1; then
    timeout 12 eu-stack -p "$pid" >"$fdir/eu-stack.txt" 2>&1 || true
  fi
  if command -v gdb >/dev/null 2>&1; then
    timeout 20 gdb -batch -ex "set pagination off" -ex "thread apply all bt" -p "$pid" \
      >"$fdir/gdb-bt.txt" 2>&1 || true
  fi
  chown -R ubuntu:ubuntu "$fdir" 2>/dev/null || true
  echo "$(ts)   ha forensics saved to $fdir" >>"$LOG"
}

heal_local() {
  local reason="$1"
  if [[ "$(restarts_in_last_hour)" -ge "$MAX_RESTARTS_PER_HOUR" ]]; then
    echo "$(ts) fleet.attester HEAL_CAP reason=${reason}" >>"$LOG"
    write_attester_status "out_of_service" "restart_cap:${reason}" "$LAST_BLUE_HEX" "" 0
    return 1
  fi
  echo "$(ts) fleet.attester HEAL_START reason=${reason} action=restart_datachain-rope" >>"$LOG"
  if [[ "$DUMP_BEFORE_RESTART" == "1" ]]; then
    capture_dump
  fi
  sudo systemctl restart datachain-rope.service
  record_restart
  FAIL_COUNT=0
  UNHEALTHY_SINCE=0
  EDGE_UNHEALTHY_SINCE=0
  save_state
  echo "$(ts) fleet.attester HEAL_ISSUED reason=${reason}" >>"$LOG"
  write_attester_status "healing" "$reason" "" "" 0
  return 0
}

write_attester_status() {
  local local_status="$1" reason="$2" local_hex="$3" writer_hex="$4" sync_lag="$5"
  local writer_status="healthy"
  [[ -z "$writer_hex" ]] && writer_status="unknown"
  if [[ "${FLEET_PUBLISH_STATUS}" != "1" ]]; then
    echo "$(ts) attester status=${local_status} lag=${sync_lag} local=${local_hex} writer=${writer_hex} (publish skipped)" >>"$LOG"
    return 0
  fi
  ROPE_HA_NODE_ROLE="attester" \
  ROPE_HA_NODE_ID="$FLEET_NODE_ID" \
  ROPE_HA_LOCAL_HEX="$local_hex" \
  ROPE_HA_WRITER_CANONICAL_HEX="$writer_hex" \
  ROPE_HA_WRITER_STATUS="$writer_status" \
  ROPE_HA_WRITER_RPC="$WRITER_RPC_URL" \
  ROPE_HA_SYNC_LAG="$sync_lag" \
  write_status "$local_status" "$reason" "$writer_hex" "" ""
}

main_attester() {
  if in_startup_grace; then
    write_attester_status "starting" "startup_grace" "" "" 0
    echo "$(ts) attester startup grace - skip heal" >>"$LOG"
    exit 0
  fi

  local local_body local_hex writer_body writer_hex local_ok writer_ok sync_lag reason
  local_body=$(blue_probe_call)
  local_hex=$(blue_probe_hex "$local_body")

  writer_body=$(rpc_call "$WRITER_RPC_URL" eth_blockNumber)
  writer_hex=$(printf '%s' "$writer_body" | parse_hex_result)

  local_ok=0
  writer_ok=0
  [[ -n "$local_hex" ]] && local_ok=1
  [[ -n "$writer_hex" ]] && writer_ok=1

  sync_lag=0
  if [[ "$local_ok" -eq 1 && "$writer_ok" -eq 1 ]]; then
    sync_lag=$(hex_block_diff "$writer_hex" "$local_hex")
  fi

  reason=""
  if [[ "$sync_lag" -gt "$SYNC_LAG_MAX_BLOCKS" ]]; then
    reason="sync_lag:${sync_lag}_blocks"
    trigger_sync_resync_if_needed "$sync_lag"
  fi

  if [[ "$local_ok" -eq 1 ]]; then
    if [[ "$local_hex" != "$LAST_BLUE_HEX" ]]; then
      LAST_BLUE_HEX="$local_hex"
      LAST_BLUE_HEX_AT=$(now_epoch)
    fi
    [[ "$LAST_BLUE_HEX_AT" -eq 0 ]] && LAST_BLUE_HEX_AT=$(now_epoch)
  fi

  if [[ "$local_ok" -eq 0 ]]; then
    FAIL_COUNT=$((FAIL_COUNT + 1))
    reason="${reason:-rpc_probe_fail}"
    if [[ -z "${UNHEALTHY_SINCE:-}" || "${UNHEALTHY_SINCE}" == "0" ]]; then
      UNHEALTHY_SINCE=$(now_epoch)
    fi
    echo "$(ts) attester UNHEALTHY attempt=${FAIL_COUNT}/${FAIL_THRESHOLD} reason=${reason} sync_lag=${sync_lag}" >>"$LOG"
    save_state
    local node_status="unhealthy"
    local effective_threshold="$FAIL_THRESHOLD"
    if [[ "$PEER_DEFER_ENABLED" == "1" && "$reason" == rpc_probe_fail* ]]; then
      local unhealthy_for=$(( $(now_epoch) - UNHEALTHY_SINCE ))
      if [[ "$unhealthy_for" -lt "$PEER_DEFER_MAX_SECS" ]]; then
        effective_threshold="$PEER_DEFER_FAIL_THRESHOLD"
        node_status="degraded"
      fi
    fi
    write_attester_status "$node_status" "$reason" "$local_hex" "$writer_hex" "$sync_lag"
    if [[ "$FAIL_COUNT" -ge "$effective_threshold" ]]; then
      heal_local "$reason" || true
    fi
    exit 0
  fi

  if [[ "$sync_lag" -gt "$SYNC_LAG_MAX_BLOCKS" ]]; then
    FAIL_COUNT=0
    UNHEALTHY_SINCE=0
    save_state
    write_attester_status "sync_lagging" "$reason" "$local_hex" "$writer_hex" "$sync_lag"
    echo "$(ts) attester sync_lagging lag=${sync_lag} local=${local_hex} writer=${writer_hex}" >>"$LOG"
    exit 0
  fi

  if [[ "$FAIL_COUNT" -ne 0 || "${UNHEALTHY_SINCE:-0}" -ne 0 ]]; then
    echo "$(ts) attester RECOVERED after ${FAIL_COUNT} fails local=${local_hex}" >>"$LOG"
  fi
  FAIL_COUNT=0
  UNHEALTHY_SINCE=0
  save_state
  write_attester_status "healthy" "ok" "$local_hex" "$writer_hex" "$sync_lag"
  exit 0
}

heal_blue() {
  local reason="$1"
  local n
  n=$(restarts_in_last_hour)
  if [[ "$n" -ge "$MAX_RESTARTS_PER_HOUR" ]]; then
    echo "$(ts) fleet.failover SKIP_RESTART reason=${reason} restarts_last_hour=${n} cap=${MAX_RESTARTS_PER_HOUR}" >>"$LOG"
    write_status "out_of_service" "restart_cap:${reason}" "$LAST_BLUE_HEX" "" "blue"
    return 1
  fi
  echo "$(ts) fleet.failover HEAL_START reason=${reason} from=blue action=restart_datachain-rope" >>"$LOG"
  if [[ "$DUMP_BEFORE_RESTART" == "1" ]]; then
    capture_dump
  fi
  sudo systemctl restart datachain-rope.service
  record_restart
  FAIL_COUNT=0
  UNHEALTHY_SINCE=0
  EDGE_UNHEALTHY_SINCE=0
  save_state
  echo "$(ts) fleet.failover HEAL_ISSUED reason=${reason} restarts_last_hour=$(restarts_in_last_hour)" >>"$LOG"
  write_status "healing" "$reason" "" "" "blue"
  return 0
}

# --- main ---
load_state

if [[ "$FLEET_NODE_ROLE" == "attester" ]]; then
  main_attester
fi

if in_startup_grace; then
  write_status "starting" "startup_grace" "" "" ""
  echo "$(ts) startup grace - skip heal" >>"$LOG"
  exit 0
fi

BLUE_BODY=$(blue_probe_call)
BLUE_HEX=$(blue_probe_hex "$BLUE_BODY")

PEER_HEX=""
PEER_OK=0
for u in $PEER_RPCS; do
  body=$(rpc_call "$u" eth_blockNumber)
  hx=$(printf '%s' "$body" | parse_hex_result)
  if [[ -n "$hx" ]]; then
    PEER_OK=1
    PEER_HEX="$hx"
    break
  fi
done

REASON=""
BLUE_OK=0

if [[ -n "$BLUE_HEX" ]]; then
  BLUE_OK=1
fi

# Stall: BLUE answers but hex frozen while a peer advanced.
if [[ "$BLUE_OK" -eq 1 && -n "$LAST_BLUE_HEX" && "$BLUE_HEX" == "$LAST_BLUE_HEX" && "$PEER_OK" -eq 1 && -n "$PEER_HEX" && "$PEER_HEX" != "$BLUE_HEX" ]]; then
  frozen_for=$(( $(now_epoch) - LAST_BLUE_HEX_AT ))
  if [[ "$frozen_for" -ge "$STALL_PEER_ADVANCE_S" ]]; then
    BLUE_OK=0
    REASON="block_stall:${frozen_for}s_blue=${BLUE_HEX}_peer=${PEER_HEX}"
  fi
fi

if [[ "$BLUE_OK" -eq 1 ]]; then
  if [[ "$BLUE_HEX" != "$LAST_BLUE_HEX" ]]; then
    LAST_BLUE_HEX="$BLUE_HEX"
    LAST_BLUE_HEX_AT=$(now_epoch)
  fi
  [[ "$LAST_BLUE_HEX_AT" -eq 0 ]] && LAST_BLUE_HEX_AT=$(now_epoch)
  if [[ "$FAIL_COUNT" -ne 0 || "${UNHEALTHY_SINCE:-0}" -ne 0 ]]; then
    echo "$(ts) RECOVERED after ${FAIL_COUNT} fails blue=${BLUE_HEX}" >>"$LOG"
  fi
  FAIL_COUNT=0
  UNHEALTHY_SINCE=0
  save_state
  # Autonomous ghost reclaim while the sealer is healthy enough to accept writes.
  reclaim_ghost_txs
  write_status "healthy" "ok" "$BLUE_HEX" "$PEER_HEX" ""
  exit 0
fi

# BLUE unhealthy
FAIL_COUNT=$((FAIL_COUNT + 1))
REASON="${REASON:-rpc_probe_fail}"
if [[ -z "${UNHEALTHY_SINCE:-}" || "${UNHEALTHY_SINCE}" == "0" ]]; then
  UNHEALTHY_SINCE=$(now_epoch)
fi
echo "$(ts) UNHEALTHY attempt=${FAIL_COUNT}/${FAIL_THRESHOLD} reason=${REASON} peer_ok=${PEER_OK} unhealthy_for=$(( $(now_epoch) - UNHEALTHY_SINCE ))s" >>"$LOG"
save_state

effective_threshold="$FAIL_THRESHOLD"
writer_status="unhealthy"
if [[ "$PEER_DEFER_ENABLED" == "1" && "$PEER_OK" -eq 1 && "$REASON" == rpc_probe_fail* ]]; then
  unhealthy_for=$(( $(now_epoch) - UNHEALTHY_SINCE ))
  if [[ "$unhealthy_for" -lt "$PEER_DEFER_MAX_SECS" ]]; then
    effective_threshold="$PEER_DEFER_FAIL_THRESHOLD"
    writer_status="degraded"
    echo "$(ts) peer_defer active threshold=${effective_threshold} max_secs=${PEER_DEFER_MAX_SECS} peer_ok=1" >>"$LOG"
  else
    echo "$(ts) peer_defer expired unhealthy_for=${unhealthy_for}s forcing heal threshold=${FAIL_THRESHOLD}" >>"$LOG"
  fi
fi
write_status "$writer_status" "$REASON" "$BLUE_HEX" "$PEER_HEX" "blue"

if [[ "$FAIL_COUNT" -ge "$effective_threshold" ]]; then
  heal_blue "$REASON" || true
fi

exit 0
