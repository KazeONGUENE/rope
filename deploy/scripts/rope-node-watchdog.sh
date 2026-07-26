#!/usr/bin/env bash
# Watchdog for datachain-rope.service.
# rope-node has a known recurring async-deadlock under memory pressure
# (see ~/rope-node-hang-2026-05-04 + 05/ forensics). Restart MTBF ~8h.
# This watchdog detects the hang via a JSON-RPC ping and restarts the
# service after 2 consecutive failures (~4 min cooldown).
#
# Installed: 2026-05-05 by the deeper-investigation deploy.
#
# 2026-07-23 fix: startup itself can legitimately take several minutes
# under load (observed ~4m48s post-ledger-load with ~39K knots on disk —
# likely O(n) knot replay/index growth, root cause not yet fixed). The
# original 2-minute cron interval + 2-consecutive-failure threshold gave
# rope-node only ~2-4 minutes to bind :8545 before being restarted, which
# is less than a cold start actually needs. That produced a self-inflicted
# infinite restart loop: every restart reset the startup clock, so the
# process was killed just before it would have finished, forever — a full
# production outage (dcscan.io intermittently serving empty
# strings/latest + transactions/latest) until an operator disabled this
# cron by hand. Fix: skip the health check entirely during a startup
# grace period, measured from the *service's* ExecMainStartTimestamp
# (not from the watchdog's own state), so a slow-but-healthy boot is
# never mistaken for a hang.

LOG=/var/log/rope-node-watchdog.log
STATE=/tmp/rope-node-watchdog.state
RPC_URL=http://127.0.0.1:8545
TIMEOUT_S=5
FAIL_THRESHOLD=2
# Observed cold start (post ledger-load) ~4m48s at ~39K knots. Give it
# comfortable headroom above that before the watchdog is even allowed to
# consider restarting; revisit downward once the O(n) startup cost is
# fixed at the root (see 30-emergency-timeout.conf on the service).
STARTUP_GRACE_S=600

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

mkdir -p "$(dirname "$STATE")" 2>/dev/null
[ -f "$STATE" ] || echo 0 > "$STATE"

# --- Startup grace period ---------------------------------------------
# ExecMainStartTimestamp is set the moment systemd forks the main
# process, whether the unit has reached "active" yet or not, so this
# works correctly even while the service is still in "activating".
start_ts_raw=$(systemctl show -p ExecMainStartTimestamp --value datachain-rope.service 2>/dev/null)
if [ -n "$start_ts_raw" ] && [ "$start_ts_raw" != "n/a" ]; then
  start_epoch=$(date -d "$start_ts_raw" +%s 2>/dev/null || echo 0)
  now_epoch=$(date -u +%s)
  if [ "$start_epoch" -gt 0 ]; then
    age=$((now_epoch - start_epoch))
    if [ "$age" -lt "$STARTUP_GRACE_S" ]; then
      echo "$(ts) in startup grace period (${age}s/${STARTUP_GRACE_S}s since last start) — skipping health check" >> "$LOG"
      exit 0
    fi
  fi
fi
# ------------------------------------------------------------------------

response=$(curl --max-time $TIMEOUT_S -sS -X POST -H "Content-Type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}" \
  "$RPC_URL" 2>&1)
exit_code=$?

if [ $exit_code -eq 0 ] && echo "$response" | grep -q "result"; then
  prev=$(cat "$STATE" 2>/dev/null || echo 0)
  if [ "$prev" != "0" ]; then
    echo "$(ts) recovered after $prev failures" >> "$LOG"
  fi
  echo 0 > "$STATE"
  exit 0
fi

prev=$(cat "$STATE" 2>/dev/null || echo 0)
new=$((prev + 1))
echo "$new" > "$STATE"
echo "$(ts) ping failed (attempt $new/$FAIL_THRESHOLD): exit=$exit_code body=$(echo "$response" | head -c 100)" >> "$LOG"

if [ "$new" -ge "$FAIL_THRESHOLD" ]; then
  pid=$(systemctl show -p MainPID --value datachain-rope 2>/dev/null)
  rss=$(ps -p "$pid" -o rss= 2>/dev/null | tr -d " ")
  echo "$(ts) RESTARTING datachain-rope.service (pid=$pid rss=$rss kB)" >> "$LOG"

  # Capture forensics first if rope-node looks deadlocked (low RSS + futex wait)
  if [ -n "$pid" ] && [ "$rss" -lt 100000 ]; then
    fdir="/home/ubuntu/rope-node-hang-$(date -u +%Y-%m-%dT%H%M%SZ)"
    mkdir -p "$fdir"
    sudo cp /proc/$pid/status "$fdir/status.txt" 2>/dev/null
    sudo cp /proc/$pid/wchan "$fdir/wchan.txt" 2>/dev/null
    for t in /proc/$pid/task/*; do
      tid=$(basename "$t")
      wchan=$(sudo cat "$t/wchan" 2>/dev/null)
      comm=$(sudo cat "$t/comm" 2>/dev/null)
      printf "tid=%s comm=%-15s wchan=%s\n" "$tid" "$comm" "$wchan"
    done > "$fdir/threads.txt" 2>/dev/null
    chown -R ubuntu:ubuntu "$fdir" 2>/dev/null
    echo "$(ts)   forensics saved to $fdir" >> "$LOG"
  fi

  sudo systemctl restart datachain-rope.service
  echo 0 > "$STATE"
fi
