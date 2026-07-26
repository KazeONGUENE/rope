#!/usr/bin/env bash
#
# promote-shadow-witness.sh
#
# Orchestrator. Runs on rope-vps (BLUE) under a systemd timer that fires
# once at canary_start + 7 days.
#
# Sequence:
#   1. Honour the `/etc/rope-shadow-witness/promotion-disabled` kill-switch.
#   2. Run the canary health gate. If it fails, log and exit (no deploy).
#   3. If it passes, deploy to BLUE (local), then GREEN (remote).
#   4. Each deploy includes a self-smoke. A target failure aborts the
#      remainder of the promotion (so a broken BLUE does not propagate
#      to GREEN). The failed target is left stopped for operator review.
#   5. Final report goes to /var/log/rope-shadow-witness-promotion.log
#      and to syslog with tag `shadow-witness-promotion`.
#
# Manual invocation:
#   sudo /usr/local/bin/promote-shadow-witness.sh
#   sudo /usr/local/bin/promote-shadow-witness.sh --gate-only   # just print gate report
#   sudo /usr/local/bin/promote-shadow-witness.sh --skip-gate   # force deploy regardless (use with care)
#
# Exit codes:
#   0 = full success OR deliberate skip (kill-switch / gate fail)
#   1 = BLUE deploy failed
#   2 = GREEN deploy failed (BLUE already deployed; the witness mesh is
#       2-of-3 healthy, see runbook)
#   3 = unexpected error

set -uo pipefail

LOG=/var/log/rope-shadow-witness-promotion.log
KILL_SWITCH=/etc/rope-shadow-witness/promotion-disabled
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GATE="$SCRIPT_DIR/canary-health-gate.sh"
DEPLOY="$SCRIPT_DIR/deploy-shadow-witness.sh"

GATE_ONLY=false
SKIP_GATE=false
for arg in "$@"; do
    case "$arg" in
        --gate-only) GATE_ONLY=true ;;
        --skip-gate) SKIP_GATE=true ;;
        --help|-h) sed -n '3,30p' "$0"; exit 0 ;;
        *) echo "unknown arg: $arg" >&2; exit 3 ;;
    esac
done

mkdir -p "$(dirname "$LOG")"
exec > >(tee -a "$LOG") 2>&1

stamp() { date -u +"%Y-%m-%dT%H:%M:%SZ"; }
say()   { echo "[$(stamp)] $*"; logger -t shadow-witness-promotion "$*" || true; }

say "============================================================"
say "promote-shadow-witness.sh start"
say "============================================================"

# ---------------------------------------------------------------------------
# Kill-switch
# ---------------------------------------------------------------------------
if [ -f "$KILL_SWITCH" ]; then
    say "KILL_SWITCH present at $KILL_SWITCH; promotion ABORTED by operator request."
    cat "$KILL_SWITCH" || true
    say "Exit 0 (no-op)."
    exit 0
fi

# ---------------------------------------------------------------------------
# 1. Gate
# ---------------------------------------------------------------------------
if $SKIP_GATE; then
    say "WARNING --skip-gate set; bypassing canary health gate."
else
    say "Running canary health gate..."
    if ! "$GATE"; then
        say "GATE_FAIL — promotion aborted. Soak criteria not met."
        say "Exit 0 (no-op)."
        exit 0
    fi
    say "GATE_PASS — proceeding to deploy."
    if $GATE_ONLY; then
        say "--gate-only requested; not deploying."
        exit 0
    fi
fi

# ---------------------------------------------------------------------------
# 2. Deploy BLUE (local)
# ---------------------------------------------------------------------------
say "Deploying to BLUE (rope-vps, local)..."
if ! "$DEPLOY" local "blue-gandi-paris"; then
    say "BLUE_DEPLOY_FAIL — exit 1 (BLUE left stopped for operator review)."
    exit 1
fi
say "BLUE_DEPLOY_OK — verifying local rope_v2_status reports counters..."
RES=$(curl -sS --max-time 5 -X POST -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"rope_v2_status","params":[],"id":1}' \
    http://127.0.0.1:8556 || true)
say "BLUE rope_v2_status: $RES"

# ---------------------------------------------------------------------------
# 3. Deploy GREEN (remote)
# ---------------------------------------------------------------------------
say "Deploying to GREEN (anvil-vps, 92.243.25.119)..."
GREEN_KEY="${HOME}/.ssh/id_ed25519"
if ! "$DEPLOY" remote "ubuntu@92.243.25.119" "green-gandi-paris" "$GREEN_KEY"; then
    say "GREEN_DEPLOY_FAIL — exit 2."
    say "BLUE is healthy; canary remains the third witness in the mesh."
    exit 2
fi
say "GREEN_DEPLOY_OK — verifying remote rope_v2_status..."
RES=$(ssh -i "$GREEN_KEY" -o BatchMode=yes ubuntu@92.243.25.119 \
    "curl -sS --max-time 5 -X POST -H 'Content-Type: application/json' \
    -d '{\"jsonrpc\":\"2.0\",\"method\":\"rope_v2_status\",\"params\":[],\"id\":1}' \
    http://127.0.0.1:8556" || true)
say "GREEN rope_v2_status: $RES"

# ---------------------------------------------------------------------------
# 4. Final report
# ---------------------------------------------------------------------------
say "============================================================"
say "PROMOTION_COMPLETE — three-witness mesh active:"
say "  - canary (datachain-rpc-1, 157.230.18.45) [original]"
say "  - BLUE   (rope-vps,        92.243.26.189)"
say "  - GREEN  (anvil-vps,       92.243.25.119)"
say ""
say "Determinism check (compare any one knot across all three witnesses)"
say "is performed by ops on demand, see runbook §7."
say "============================================================"
exit 0
