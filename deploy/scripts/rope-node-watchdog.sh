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
#
# 2026-07-26 fix (DCSwap-reported 5xx burst investigation): forensics
# capture is unconditional on every watchdog-triggered restart. The old
# RSS < 100MB gate never fired for the real hang signature (steady-state
# RSS 400MB–2GB).
#
# 2026-07-27 P1: when a symbolicated binary (or companion debug file)
# is present, run addr2line over PCs from gdb-bt.txt so the next hang
# dump is human-readable without a separate symbol server.
#
# 2026-07-27 P1.1 critique: SIGKILL was destroying the evidence that
# distinguishes a recoverable *stall* (compaction / thundering herd,
# recovers in 30–60s) from a true *deadlock*. Set
# ROPE_WATCHDOG_DUMP_ONLY=1 to capture forensics and keep the process
# alive. Prefer `eu-stack` over `gcore` (dumping 2 GB cores perturbs
# the measurement). Also: the ~840s "hang interval" in logs is often
# STARTUP_GRACE_S(600) + 2× cron period — not a chain epoch.
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
# How many forensic dumps to retain. Oldest is pruned each run.
FORENSICS_KEEP=40
# Prefer an unstripped forensic build if operators leave one beside the
# running binary; fall back to the live MainPID exe.
ROPE_BIN_DEFAULT=/home/ubuntu/datachain-rope/target/release/rope
ROPE_BIN_SYMS=/home/ubuntu/datachain-rope/target/release/rope.syms
# 1/true/yes/on → dump forensics without restart (diagnosis only).
# 2026-07-28: DUMP_ONLY left on after P1.4 soak caused multi-hour MetaMask
# outages (wedged process, no heal). Production cron must leave this unset.
# If DUMP_ONLY is on, escalate to restart after DUMP_ONLY_MAX dump cycles
# (default 2 ≈ 4–8 min) so soak/diagnosis cannot strand the public edge.
DUMP_ONLY="${ROPE_WATCHDOG_DUMP_ONLY:-0}"
DUMP_ONLY_MAX="${ROPE_WATCHDOG_DUMP_ONLY_MAX:-2}"
DUMP_ONLY_COUNT_FILE=/tmp/rope-node-watchdog.dump_only_count

ts() { date -u +%Y-%m-%dT%H:%M:%SZ; }

is_truthy() {
  case "$(echo "$1" | tr '[:upper:]' '[:lower:]')" in
    1|true|yes|on) return 0 ;;
    *) return 1 ;;
  esac
}

capture_forensics() {
  local pid="$1"
  local fdir="$2"
  local mode="$3"
  mkdir -p "$fdir" 2>/dev/null
  {
    echo "captured_at=$(ts)"
    echo "pid=$pid"
    echo "rss_kb=${rss:-unknown}"
    echo "watchdog_fail_threshold=$FAIL_THRESHOLD"
    echo "dump_only=$mode"
    echo "note=prefer non-futex waiters; one hot futex addr ⇒ one global lock"
  } > "$fdir/meta.txt" 2>/dev/null
  sudo cp /proc/$pid/status "$fdir/status.txt" 2>/dev/null
  sudo cp /proc/$pid/wchan "$fdir/wchan.txt" 2>/dev/null
  sudo cp /proc/$pid/stack "$fdir/main-stack.txt" 2>/dev/null

  for t in /proc/$pid/task/*; do
    tid=$(basename "$t")
    wchan=$(sudo cat "$t/wchan" 2>/dev/null)
    comm=$(sudo cat "$t/comm" 2>/dev/null)
    printf "tid=%s comm=%-15s wchan=%s\n" "$tid" "$comm" "$wchan"
  done > "$fdir/threads.txt" 2>/dev/null

  # Futex address histogram — one hot address means one global lock.
  {
    for t in /proc/$pid/task/*; do
      sudo awk '{print $2}' "$t/syscall" 2>/dev/null
    done | sort | uniq -c | sort -rn | head -40
  } > "$fdir/futex-addrs.txt" 2>/dev/null

  # Prefer eu-stack (fast whole-process stacks) over gdb/gcore.
  if command -v eu-stack >/dev/null 2>&1; then
    sudo timeout 15 eu-stack -p "$pid" > "$fdir/eu-stack.txt" 2>&1
  fi
  if command -v gdb >/dev/null 2>&1; then
    sudo timeout 10 gdb -p "$pid" -batch -ex "thread apply all bt" \
      > "$fdir/gdb-bt.txt" 2>&1
  fi

  # Symbolicate PCs when a debug-info binary is available.
  # IMPORTANT: rope.syms must come from the SAME build as the running
  # binary (objcopy --only-keep-debug + --add-gnu-debuglink), not a
  # second cargo profile build.
  sym_bin=""
  if [ -e "$ROPE_BIN_SYMS" ]; then
    sym_bin="$ROPE_BIN_SYMS"
  elif [ -x "$ROPE_BIN_DEFAULT" ]; then
    if command -v nm >/dev/null 2>&1 && nm -C "$ROPE_BIN_DEFAULT" 2>/dev/null | head -1 | grep -q .; then
      sym_bin="$ROPE_BIN_DEFAULT"
    fi
  fi
  stack_src=""
  if [ -f "$fdir/eu-stack.txt" ]; then
    stack_src="$fdir/eu-stack.txt"
  elif [ -f "$fdir/gdb-bt.txt" ]; then
    stack_src="$fdir/gdb-bt.txt"
  fi
  if [ -n "$sym_bin" ] && [ -n "$stack_src" ] && command -v addr2line >/dev/null 2>&1; then
    {
      echo "symbol_binary=$sym_bin"
      echo "stack_source=$stack_src"
      grep -oE '0x[0-9a-fA-F]{6,}' "$stack_src" | sort -u | head -80 \
        | while read -r addr; do
            echo "=== $addr ==="
            addr2line -e "$sym_bin" -f -C -p "$addr" 2>/dev/null || true
          done
    } > "$fdir/addr2line.txt" 2>/dev/null
  fi

  chown -R ubuntu:ubuntu "$fdir" 2>/dev/null
  echo "$(ts)   forensics saved to $fdir (mode=$mode)" >> "$LOG"

  ls -1dt /home/ubuntu/rope-node-hang-* 2>/dev/null \
    | tail -n +"$((FORENSICS_KEEP + 1))" \
    | xargs -r rm -rf
}

mkdir -p "$(dirname "$STATE")" 2>/dev/null
[ -f "$STATE" ] || echo 0 > "$STATE"

# --- Startup grace period ---------------------------------------------
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
    if is_truthy "$DUMP_ONLY"; then
      echo "$(ts) RECOVERED_WITHOUT_RESTART after $prev failures (stall candidate — dump-only mode)" >> "$LOG"
    else
      echo "$(ts) recovered after $prev failures" >> "$LOG"
    fi
  fi
  echo 0 > "$STATE"
  echo 0 > "$DUMP_ONLY_COUNT_FILE"
  exit 0
fi

prev=$(cat "$STATE" 2>/dev/null || echo 0)
new=$((prev + 1))
echo "$new" > "$STATE"
echo "$(ts) ping failed (attempt $new/$FAIL_THRESHOLD): exit=$exit_code body=$(echo "$response" | head -c 100)" >> "$LOG"

if [ "$new" -ge "$FAIL_THRESHOLD" ]; then
  pid=$(systemctl show -p MainPID --value datachain-rope 2>/dev/null)
  rss=$(ps -p "$pid" -o rss= 2>/dev/null | tr -d " ")

  if [ -n "$pid" ] && [ "$pid" != "0" ]; then
    fdir="/home/ubuntu/rope-node-hang-$(date -u +%Y-%m-%dT%H%M%SZ)"
    if is_truthy "$DUMP_ONLY"; then
      dump_n=$(cat "$DUMP_ONLY_COUNT_FILE" 2>/dev/null || echo 0)
      dump_n=$((dump_n + 1))
      echo "$dump_n" > "$DUMP_ONLY_COUNT_FILE"
      echo "$(ts) DUMP_ONLY forensics (pid=$pid rss=${rss:-unknown} kB) cycle=${dump_n}/${DUMP_ONLY_MAX}" >> "$LOG"
      capture_forensics "$pid" "$fdir" "dump_only"
      echo 0 > "$STATE"
      if [ "$dump_n" -ge "$DUMP_ONLY_MAX" ]; then
        echo "$(ts) DUMP_ONLY_ESCALATE → RESTARTING after ${dump_n} dump cycles (public edge must self-heal)" >> "$LOG"
        capture_forensics "$pid" "${fdir}-escalate" "dump_only_escalate"
        sudo systemctl restart datachain-rope.service
        echo 0 > "$DUMP_ONLY_COUNT_FILE"
        echo 0 > "$STATE"
      else
        echo "$(ts)   dump-only: deferring restart (${dump_n}/${DUMP_ONLY_MAX})" >> "$LOG"
      fi
    else
      echo "$(ts) RESTARTING datachain-rope.service (pid=$pid rss=${rss:-unknown} kB)" >> "$LOG"
      capture_forensics "$pid" "$fdir" "restart"
      sudo systemctl restart datachain-rope.service
      echo 0 > "$STATE"
      echo 0 > "$DUMP_ONLY_COUNT_FILE"
    fi
  else
    echo "$(ts) FAIL_THRESHOLD reached but MainPID unavailable" >> "$LOG"
    if ! is_truthy "$DUMP_ONLY"; then
      sudo systemctl restart datachain-rope.service
      echo 0 > "$STATE"
    fi
  fi
fi
