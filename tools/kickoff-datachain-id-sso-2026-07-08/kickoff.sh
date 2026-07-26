#!/usr/bin/env bash
#
# Handover Kickoff Dispatcher — bash fallback (schema v2)
# =======================================================
#
# Portable companion to kickoff.ts. Reads workspaces.json + primer.template.md
# in the same directory and fans out one Cursor agent per workspace.
#
# PORTABILITY NOTE — bash 3 vs bash 4:
#   This script prefers `wait -n` (bash 4+) for max-parallel scheduling. On
#   macOS the system /bin/bash is 3.2, and that line emits a warning + falls
#   back to `wait` (blocking on the whole batch). Functionally fine, just a
#   bit less efficient. To get the bash-4 path, install via Homebrew:
#       brew install bash
#       which bash    # /opt/homebrew/bin/bash or /usr/local/bin/bash
#   then either edit the shebang above to that path, or call:
#       /opt/homebrew/bin/bash ./kickoff.sh --dry-run
#
# Original implementation (canonical-agents handover, 2026-05-10):
#   /Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/datachain-rope/tools/
#     kickoff-handover-canonical-agents-2026-05-10/kickoff.sh
#
# =========================================================================
#
# Sister to kickoff.ts. Same workspaces.json, same primer.template.md.
# Uses the `cursor-agent` CLI (installed at ~/.local/bin/cursor-agent)
# instead of the @cursor/sdk Node package — no `npm install` needed.
#
# Schema v2 (refactored 2026-05-10 after the tanastok deploy lesson):
#   * Mode-aware idempotency: --emission honours emission_signals;
#     --deploy honours deploy_signals; --full requires both.
#   * Audited per-project deploy_command pinned in workspaces.json.
#   * Pre-flight gate: disk free / ssh keys / human_setup_required.
#   * --direct-deploy mode bypasses the agent and runs the audited
#     script directly (the manual recovery path, formalised).
#   * --force / --force-deploy / --skip-preflight escape hatches.
#
# Modes (mutually exclusive):
#   --dry-run         Print pre-flight + plan-per-workspace; don't launch agents.
#   --plan-only       Launch agents; STEP 0 + STEP 1 only.
#   --emission        Launch agents; through STEP 3 (commit + push, NO deploy).
#   --deploy          Launch agents; STEP 0 + STEP 4-6 (deploy + verify + marker).
#   --full            All steps end-to-end.
#   --direct-deploy   Bypass agent. Run audited deploy_command directly.
#   --status          Walk logs/, print summary.
#
# Flags:
#   --workspace=<name>     Run a single workspace.
#   --concurrency=<N>      Parallelism. Default 2 (1 for --direct-deploy).
#   --model=<id>           Cursor model. Default composer-2.
#   --force                Bypass marker check entirely.
#   --force-deploy         Bypass marker only in deploy/direct-deploy/full modes.
#   --skip-preflight       Skip disk / ssh / human_setup checks.
#
# Environment:
#   CURSOR_API_KEY=<key>   Required for any agent-driven mode.
#                          NOT required for --direct-deploy or --dry-run.
#
# Output:
#   logs/run-<iso>.log       Combined stdout+stderr from each run.
#   logs/run-<iso>.jsonl     One line per workspace, machine-readable.

set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACES_JSON="$SCRIPT_DIR/workspaces.json"
PRIMER_TEMPLATE="$SCRIPT_DIR/primer.template.md"
LOG_DIR="$SCRIPT_DIR/logs"
RUN_TS="$(date -u +%Y-%m-%dT%H-%M-%SZ)"
RUN_LOG_TXT="$LOG_DIR/run-$RUN_TS.log"
RUN_LOG_JSONL="$LOG_DIR/run-$RUN_TS.jsonl"

# Defaults
MODE=""
WS_FILTER=""
CONCURRENCY=2
MODEL="composer-2"
FORCE=0
FORCE_DEPLOY=0
SKIP_PREFLIGHT=0

need() {
  command -v "$1" >/dev/null 2>&1 || { echo "ERROR: missing dependency: $1" >&2; exit 1; }
}
need jq
need python3
need curl

mkdir -p "$LOG_DIR"

# ---- arg parsing ----
set_mode() {
  if [[ -n "$MODE" ]]; then
    echo "ERROR: multiple modes specified; pick one: --dry-run, --plan-only, --emission, --deploy, --full, --direct-deploy, --status" >&2
    exit 1
  fi
  MODE="$1"
}

for arg in "$@"; do
  case "$arg" in
    --dry-run)         set_mode "dry-run" ;;
    --plan-only)       set_mode "plan-only" ;;
    --emission)        set_mode "emission" ;;
    --deploy)          set_mode "deploy" ;;
    --full)            set_mode "full" ;;
    --direct-deploy)   set_mode "direct-deploy" ;;
    --status)          set_mode "status" ;;
    --force)           FORCE=1 ;;
    --force-deploy)    FORCE_DEPLOY=1 ;;
    --skip-preflight)  SKIP_PREFLIGHT=1 ;;
    --workspace=*)     WS_FILTER="${arg#--workspace=}" ;;
    --concurrency=*)   CONCURRENCY="${arg#--concurrency=}" ;;
    --model=*)         MODEL="${arg#--model=}" ;;
    *)
      echo "ERROR: unknown flag: $arg" >&2
      exit 1
      ;;
  esac
done

if [[ -z "$MODE" ]]; then
  echo "ERROR: no mode specified. Pick one of: --dry-run, --plan-only, --emission, --deploy, --full, --direct-deploy, --status" >&2
  exit 1
fi

# Direct-deploy is sequential (concurrent SSH/rsync from one laptop fights itself)
if [[ "$MODE" == "direct-deploy" ]]; then
  CONCURRENCY=1
fi

# ---- status mode ----
if [[ "$MODE" == "status" ]]; then
  shopt -s nullglob
  files=("$LOG_DIR"/run-*.jsonl)
  if (( ${#files[@]} == 0 )); then
    echo "No previous runs."
    exit 0
  fi
  for f in "${files[@]}"; do
    lines=$(wc -l < "$f" | tr -d ' ')
    echo ""
    echo "=== $(basename "$f") ($lines runs) ==="
    while IFS= read -r line; do
      [[ -z "$line" ]] && continue
      ws=$(jq -r '.workspace' <<<"$line")
      m=$(jq -r '.mode' <<<"$line")
      st=$(jq -r '.status' <<<"$line")
      d=$(jq -r '.durationMs // empty' <<<"$line")
      err=$(jq -r '.error // empty' <<<"$line")
      printf "  %-16s %-13s %-28s %s %s\n" "$ws" "$m" "$st" "${d:+"$((d/1000))s"}" "$err"
    done < "$f"
  done
  exit 0
fi

# ---- auth check (skip in dry-run and direct-deploy) ----
# cursor-agent works with either CURSOR_API_KEY or an interactive login
# (`cursor-agent login`). Accept whichever is present.
if [[ "$MODE" != "dry-run" ]] && [[ "$MODE" != "direct-deploy" ]] && [[ -z "${CURSOR_API_KEY:-}" ]]; then
  if ! cursor-agent status 2>/dev/null | grep -q "Logged in"; then
    echo "ERROR: no cursor-agent auth. Set CURSOR_API_KEY (https://cursor.com/dashboard/cloud-agents) or run 'cursor-agent login'." >&2
    exit 1
  fi
fi
if [[ "$MODE" != "dry-run" ]] && [[ "$MODE" != "direct-deploy" ]]; then
  need cursor-agent
fi

# ---- load workspaces ----
if [[ ! -f "$WORKSPACES_JSON" ]]; then
  echo "ERROR: workspaces.json not found at $WORKSPACES_JSON" >&2
  exit 1
fi
if [[ ! -f "$PRIMER_TEMPLATE" ]]; then
  echo "ERROR: primer.template.md not found at $PRIMER_TEMPLATE" >&2
  exit 1
fi

HANDOVER_REF=$(jq -r '.handover_reference' "$WORKSPACES_JSON")
IMPL_MARKER=$(jq -r '.implemented_marker' "$WORKSPACES_JSON")
BLOCKED_REPORT=$(jq -r '.blocked_report' "$WORKSPACES_JSON")
PLAN_FILE=$(jq -r '.plan_file' "$WORKSPACES_JSON")
DEFAULT_MIN_DISK=$(jq -r '.default_min_disk_gib // 5' "$WORKSPACES_JSON")
SCHEMA_VERSION=$(jq -r '.schema_version // 2' "$WORKSPACES_JSON")

if [[ -n "$WS_FILTER" ]]; then
  WORKSPACES=$(jq -c --arg n "$WS_FILTER" '.workspaces[] | select(.name == $n)' "$WORKSPACES_JSON")
else
  WORKSPACES=$(jq -c '.workspaces[]' "$WORKSPACES_JSON")
fi

if [[ -z "$WORKSPACES" ]]; then
  echo "ERROR: no workspaces match --workspace=$WS_FILTER" >&2
  echo "Known: $(jq -r '.workspaces | map(.name) | join(", ")' "$WORKSPACES_JSON")" >&2
  exit 1
fi

# ---- helpers ----

# Free disk in GiB at a given path. Output: integer GiB.
free_disk_gib() {
  local path="$1"
  df -P "$path" 2>/dev/null | awk 'NR==2 {print int($4/1048576)}'
}

# Expand ~ to $HOME in a path string.
expand_home() {
  local p="$1"
  printf '%s' "${p/#\~/$HOME}"
}

# Read structured marker. Returns "emission|deploy" with each set to 1 or 0.
read_marker() {
  local marker_path="$1"
  if [[ ! -f "$marker_path" ]]; then
    echo "0|0|0"  # exists | emission | deploy
    return
  fi
  local content
  content=$(cat "$marker_path")
  local has_emission=0 has_deploy=0
  # Accept both schema-v2 canonical and the legacy variant (## Deploy phase / ## Emission phase).
  if grep -qiE '^##[[:space:]]*(Phase:[[:space:]]*emission|Emission[[:space:]]+phase)' "$marker_path"; then has_emission=1; fi
  if grep -qiE '^##[[:space:]]*(Phase:[[:space:]]*deploy|Deploy[[:space:]]+phase)' "$marker_path"; then has_deploy=1; fi
  # v1 free-form marker: treat as emission-only.
  if [[ "$has_emission" == "0" ]] && [[ "$has_deploy" == "0" ]]; then
    has_emission=1
  fi
  echo "1|$has_emission|$has_deploy"
}

# Decide if marker is enough to skip in this mode.
should_skip() {
  local mode="$1"
  local exists="$2" emission="$3" deploy="$4"
  if [[ "$exists" == "0" ]]; then echo "0"; return; fi
  if [[ "$FORCE" == "1" ]]; then echo "0"; return; fi
  if [[ "$FORCE_DEPLOY" == "1" ]] && [[ "$mode" =~ ^(deploy|direct-deploy|full)$ ]]; then echo "0"; return; fi
  case "$mode" in
    emission)
      [[ "$emission" == "1" ]] && echo "1" || echo "0" ;;
    deploy|direct-deploy)
      [[ "$deploy" == "1" ]] && echo "1" || echo "0" ;;
    full)
      [[ "$emission" == "1" ]] && [[ "$deploy" == "1" ]] && echo "1" || echo "0" ;;
    *)
      echo "0" ;;
  esac
}

# Pre-flight gate. Echoes "ok" or "BLOCK: <reason>"; warnings to stderr.
preflight() {
  local ws_json="$1" mode="$2"
  local cwd name min_disk_gib
  cwd=$(jq -r '.cwd' <<<"$ws_json")
  name=$(jq -r '.name' <<<"$ws_json")
  min_disk_gib=$(jq -r ".min_disk_gib // ${DEFAULT_MIN_DISK}" <<<"$ws_json")

  if [[ "$SKIP_PREFLIGHT" == "1" ]]; then echo "ok"; return; fi

  local blocking_modes="emission deploy full direct-deploy"
  if [[ "$blocking_modes" == *"$mode"* ]]; then
    local free
    free=$(free_disk_gib "$cwd")
    if [[ -n "$free" ]] && [[ "$free" -lt "$min_disk_gib" ]]; then
      echo "BLOCK: disk free is ${free} GiB at $cwd; project requires ≥ ${min_disk_gib} GiB"
      return
    fi
    if [[ "$mode" =~ ^(deploy|full|direct-deploy)$ ]]; then
      local keys
      keys=$(jq -r '.ssh_keys_required // [] | .[]' <<<"$ws_json")
      while IFS= read -r key; do
        [[ -z "$key" ]] && continue
        local expanded
        expanded=$(expand_home "$key")
        if [[ ! -f "$expanded" ]]; then
          echo "BLOCK: SSH key required by deploy_command not found: $key"
          return
        fi
      done <<<"$keys"
    fi
  fi

  local human_setup
  human_setup=$(jq -r '.human_setup_required // false' <<<"$ws_json")
  if [[ "$human_setup" == "true" ]] && [[ "$mode" =~ ^(deploy|full|direct-deploy)$ ]]; then
    echo "WARN: human_setup_required=true — deploy will be deferred (see human_setup_checklist)" >&2
  fi

  local deploy_cmd
  deploy_cmd=$(jq -r '.deploy_command // "null"' <<<"$ws_json")
  if [[ "$deploy_cmd" == "null" ]] && [[ "$mode" =~ ^(deploy|full|direct-deploy)$ ]]; then
    echo "WARN: deploy_command is null — deploy phase will write a deferred-deploy blocker" >&2
  fi

  if [[ "$mode" =~ ^(emission|full)$ ]] && [[ -d "$cwd/.git" ]]; then
    if [[ -z "$(cd "$cwd" 2>/dev/null && git remote 2>/dev/null)" ]]; then
      echo "WARN: no git remote configured — STEP 3 will commit locally only, no push" >&2
    fi
  fi

  echo "ok"
}

# ---- build primer for one workspace (only used in agent modes) ----
build_primer() {
  local ws_json="$1"
  local mode_for_primer="$2"

  local name cwd sensitivity primer_extra deploy_command deploy_command_notes
  name=$(jq -r '.name' <<<"$ws_json")
  cwd=$(jq -r '.cwd' <<<"$ws_json")
  sensitivity=$(jq -r '.sensitivity' <<<"$ws_json")
  primer_extra=$(jq -r '.primer_extra' <<<"$ws_json")
  deploy_command=$(jq -r '.deploy_command // "null"' <<<"$ws_json")
  deploy_command_notes=$(jq -r '.deploy_command_notes // "(no notes recorded)"' <<<"$ws_json")

  local actions emission_signals deploy_signals
  actions=$(jq -r '.actions | to_entries | map("   \(.key+1). \(.value)") | join("\n")' <<<"$ws_json")
  emission_signals=$(jq -r '(.emission_signals // .already_done_signals // []) | map("   - \(.)") | join("\n")' <<<"$ws_json")
  deploy_signals=$(jq -r '(.deploy_signals // .already_done_signals // []) | map("   - \(.)") | join("\n")' <<<"$ws_json")
  [[ -z "$emission_signals" ]] && emission_signals="   (none recorded)"
  [[ -z "$deploy_signals" ]] && deploy_signals="   (none recorded)"

  local production_check human_setup_required human_setup_checklist
  production_check=$(jq '.production_check // null' <<<"$ws_json")
  human_setup_required=$(jq -r '.human_setup_required // false' <<<"$ws_json")
  human_setup_checklist=$(jq -r '(.human_setup_checklist // []) | to_entries | map("   \(.key+1). \(.value)") | join("\n")' <<<"$ws_json")
  [[ -z "$human_setup_checklist" ]] && human_setup_checklist="   (none)"

  python3 - <<PY
import sys
with open("$PRIMER_TEMPLATE","r") as f: t=f.read()
def sub(t,k,v): return t.replace("{{"+k+"}}", v)
out=t
out=sub(out,"NAME", $(jq -Rsa . <<<"$name"))
out=sub(out,"CWD", $(jq -Rsa . <<<"$cwd"))
out=sub(out,"HANDOVER_PATH", $(jq -Rsa . <<<"$HANDOVER_REF"))
out=sub(out,"IMPLEMENTED_MARKER", $(jq -Rsa . <<<"$IMPL_MARKER"))
out=sub(out,"BLOCKED_REPORT", $(jq -Rsa . <<<"$BLOCKED_REPORT"))
out=sub(out,"PLAN_FILE", $(jq -Rsa . <<<"$PLAN_FILE"))
out=sub(out,"MODE", $(jq -Rsa . <<<"$mode_for_primer"))
out=sub(out,"SENSITIVITY", $(jq -Rsa . <<<"$sensitivity"))
out=sub(out,"ACTIONS", $(jq -Rsa . <<<"$actions"))
out=sub(out,"EMISSION_SIGNALS", $(jq -Rsa . <<<"$emission_signals"))
out=sub(out,"DEPLOY_SIGNALS", $(jq -Rsa . <<<"$deploy_signals"))
out=sub(out,"DEPLOY_COMMAND", $(jq -Rsa . <<<"$deploy_command"))
out=sub(out,"DEPLOY_COMMAND_NOTES", $(jq -Rsa . <<<"$deploy_command_notes"))
out=sub(out,"PRODUCTION_CHECK", $(jq -Rsa . <<<"$production_check"))
out=sub(out,"HUMAN_SETUP_REQUIRED", $(jq -Rsa . <<<"$human_setup_required"))
out=sub(out,"HUMAN_SETUP_CHECKLIST", $(jq -Rsa . <<<"$human_setup_checklist"))
out=sub(out,"PRIMER_EXTRA", $(jq -Rsa . <<<"$primer_extra"))
sys.stdout.write(out)
PY
}

# ---- direct-deploy: bypass agent and run the audited script ----
run_direct_deploy() {
  local ws_json="$1"
  local name cwd deploy_command
  name=$(jq -r '.name' <<<"$ws_json")
  cwd=$(jq -r '.cwd' <<<"$ws_json")
  deploy_command=$(jq -r '.deploy_command // "null"' <<<"$ws_json")
  local started_at
  started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)

  if [[ "$deploy_command" == "null" ]]; then
    echo "[$name] DEPLOY-SKIP deploy_command is null" | tee -a "$RUN_LOG_TXT"
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "skipped-no-deploy-command" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, error:"deploy_command is null"}' >> "$RUN_LOG_JSONL"
    return
  fi

  local human_setup
  human_setup=$(jq -r '.human_setup_required // false' <<<"$ws_json")
  if [[ "$human_setup" == "true" ]]; then
    echo "[$name] DEPLOY-DEFERRED human_setup_required=true" | tee -a "$RUN_LOG_TXT"
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "skipped-no-deploy-command" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, error:"human_setup_required=true"}' >> "$RUN_LOG_JSONL"
    return
  fi

  echo "[$name] DIRECT-DEPLOY cwd=$cwd cmd=$deploy_command" | tee -a "$RUN_LOG_TXT"
  local t0 t1 ec
  t0=$(date +%s)
  ( cd "$cwd" && bash -c "$deploy_command" ) 2>&1 | tee -a "$RUN_LOG_TXT"
  ec=${PIPESTATUS[0]}
  t1=$(date +%s)
  local duration_s=$((t1 - t0))

  if [[ "$ec" != "0" ]]; then
    echo "[$name] DEPLOY-FAIL exit=$ec duration=${duration_s}s" | tee -a "$RUN_LOG_TXT"
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "deploy-failed" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson dms "$((duration_s * 1000))" --arg err "exit=$ec" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, durationMs:$dms, error:$err}' >> "$RUN_LOG_JSONL"
    return
  fi

  # Production verification
  local pc_url pc_status pc_follow ssh_check
  pc_url=$(jq -r '.production_check.url // ""' <<<"$ws_json")
  pc_status=$(jq -r '.production_check.expected_status // 200' <<<"$ws_json")
  pc_follow=$(jq -r '.production_check.follow_redirects // true' <<<"$ws_json")
  ssh_check=$(jq -r '.production_check.ssh_state_check // ""' <<<"$ws_json")

  local evidence=""
  local verify_ok=1
  if [[ -n "$pc_url" ]] && [[ "$pc_url" != "null" ]]; then
    local follow_flag=""
    [[ "$pc_follow" == "true" ]] && follow_flag="-L"
    local code
    code=$(curl -sS -o /dev/null -w "%{http_code}" $follow_flag --max-time 20 "$pc_url" 2>/dev/null || echo "0")
    evidence+="$pc_url -> HTTP $code (expected $pc_status); "
    if [[ "$code" != "$pc_status" ]]; then verify_ok=0; fi
  fi
  if [[ -n "$ssh_check" ]] && [[ "$ssh_check" != "null" ]]; then
    local ssh_out
    if ssh_out=$(eval "$ssh_check" 2>&1); then
      evidence+="ssh_check -> $(printf '%s' "$ssh_out" | tr '\n' ' ' | head -c 150); "
    else
      evidence+="ssh_check FAILED; "
      verify_ok=0
    fi
  fi

  if [[ "$verify_ok" != "1" ]]; then
    echo "[$name] VERIFY-FAIL $evidence" | tee -a "$RUN_LOG_TXT"
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "verify-failed" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson dms "$((duration_s * 1000))" --arg err "$evidence" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, durationMs:$dms, error:$err}' >> "$RUN_LOG_JSONL"
    return
  fi

  # Append a structured marker entry
  local marker_path="$cwd/$IMPL_MARKER"
  local now
  now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  {
    echo "$now"
    echo ""
    echo "## Phase: deploy"
    echo "- Deploy command: $deploy_command"
    echo "- Mode: --direct-deploy (bash dispatcher; no agent in the loop)"
    echo "- Finished: $now"
    echo "- Production verification:"
    echo "  - $evidence"
    echo ""
    echo "## Result"
    echo "- Deploy: SHIPPED via $deploy_command"
    echo "- Verification: passed"
    echo ""
  } >> "$marker_path"

  echo "[$name] DEPLOY-SUCCESS duration=${duration_s}s evidence=$evidence" | tee -a "$RUN_LOG_TXT"
  jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "deploy-success" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --argjson dms "$((duration_s * 1000))" \
    '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, durationMs:$dms}' >> "$RUN_LOG_JSONL"
}

# ---- run one workspace ----
run_one() {
  local ws_json="$1"
  local name cwd
  name=$(jq -r '.name' <<<"$ws_json")
  cwd=$(jq -r '.cwd' <<<"$ws_json")

  local started_at
  started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)

  if [[ ! -d "$cwd" ]]; then
    echo "[$name] STARTUP-FAIL workspace missing at $cwd" | tee -a "$RUN_LOG_TXT" >&2
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "skipped-missing-cwd" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg err "workspace not found at $cwd" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, error:$err}' >> "$RUN_LOG_JSONL"
    return
  fi

  # Pre-flight gate
  local pf
  pf=$(preflight "$ws_json" "$MODE" 2>>"$RUN_LOG_TXT")
  if [[ "$pf" != "ok" ]]; then
    echo "[$name] PREFLIGHT-BLOCK $pf" | tee -a "$RUN_LOG_TXT" >&2
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "skipped-preflight-blocker" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg err "$pf" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, error:$err}' >> "$RUN_LOG_JSONL"
    return
  fi

  # Idempotency: read structured marker and decide
  local marker_path="$cwd/$IMPL_MARKER"
  local m_state ex em dp
  m_state=$(read_marker "$marker_path")
  ex=$(echo "$m_state" | cut -d'|' -f1)
  em=$(echo "$m_state" | cut -d'|' -f2)
  dp=$(echo "$m_state" | cut -d'|' -f3)
  local skip
  skip=$(should_skip "$MODE" "$ex" "$em" "$dp")
  if [[ "$skip" == "1" ]]; then
    echo "[$name] SKIP (marker satisfies $MODE: emission=$em deploy=$dp; --force or --force-deploy to override)" | tee -a "$RUN_LOG_TXT"
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "skipped-already-implemented" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" --arg err "marker already satisfies mode" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, error:$err}' >> "$RUN_LOG_JSONL"
    return
  fi

  # Direct-deploy short-circuits agent path
  if [[ "$MODE" == "direct-deploy" ]]; then
    run_direct_deploy "$ws_json"
    return
  fi

  local mode_for_primer="$MODE"
  [[ "$MODE" == "dry-run" ]] && mode_for_primer="plan-only"

  local primer
  primer=$(build_primer "$ws_json" "$mode_for_primer")

  if [[ "$MODE" == "dry-run" ]]; then
    echo "[$name] DRY-RUN cwd=$cwd primer_chars=${#primer} marker=exists:$ex emission:$em deploy:$dp" | tee -a "$RUN_LOG_TXT"
    jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "skipped-dry-run" --arg sa "$started_at" --arg ea "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
      '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea}' >> "$RUN_LOG_JSONL"
    return
  fi

  echo "[$name] LAUNCH cwd=$cwd mode=$MODE" | tee -a "$RUN_LOG_TXT"
  local t0 t1 status_str
  t0=$(date +%s)
  local out
  if out=$(cursor-agent --print --workspace "$cwd" --model "$MODEL" -f --sandbox disabled --output-format text "$primer" 2>&1); then
    status_str="finished"
  else
    status_str="error"
  fi
  t1=$(date +%s)
  local ended_at
  ended_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  local duration_s=$((t1 - t0))

  echo "----- [$name] output begin -----" >> "$RUN_LOG_TXT"
  printf '%s\n' "$out" >> "$RUN_LOG_TXT"
  echo "----- [$name] output end -----" >> "$RUN_LOG_TXT"

  echo "[$name] $(echo "$status_str" | tr '[:lower:]' '[:upper:]') duration=${duration_s}s" | tee -a "$RUN_LOG_TXT"
  jq -nc --arg ws "$name" --arg mode "$MODE" --arg s "$status_str" --arg sa "$started_at" --arg ea "$ended_at" --argjson dms "$((duration_s * 1000))" \
    '{workspace:$ws, mode:$mode, status:$s, startedAt:$sa, endedAt:$ea, durationMs:$dms}' >> "$RUN_LOG_JSONL"
}

# ---- main loop with bounded concurrency ----
HANDOVER_TITLE="$(jq -r '.handover_reference // "unspecified-handover"' "$WORKSPACES_JSON" | xargs -I{} basename {} .mdc)"
echo "Handover Kickoff Dispatcher [$HANDOVER_TITLE, schema v$SCHEMA_VERSION] — mode=$MODE concurrency=$CONCURRENCY model=$MODEL force=$FORCE force-deploy=$FORCE_DEPLOY skip-preflight=$SKIP_PREFLIGHT" | tee -a "$RUN_LOG_TXT"
echo "Run log:        $RUN_LOG_TXT"
echo "Run log jsonl:  $RUN_LOG_JSONL"
echo ""

# Detect `wait -n` support (bash 4+) once, fall back cleanly on bash 3.x (macOS default /bin/bash is 3.2).
if [[ "${BASH_VERSINFO[0]:-0}" -ge 4 ]]; then
  HAVE_WAIT_N=1
else
  HAVE_WAIT_N=0
  echo "  (note: bash ${BASH_VERSION%%.*}.x detected; falling back to whole-batch wait. Install bash 4+ via Homebrew for max parallelism — see header comment.)" | tee -a "$RUN_LOG_TXT"
fi

in_flight=0
while IFS= read -r ws_json; do
  run_one "$ws_json" &
  in_flight=$((in_flight + 1))
  if (( in_flight >= CONCURRENCY )); then
    if (( HAVE_WAIT_N )); then
      wait -n
      in_flight=$((in_flight - 1))
    else
      wait
      in_flight=0
    fi
  fi
done <<<"$WORKSPACES"
wait

# ---- summary ----
echo ""
echo "=== summary ==="
if [[ -f "$RUN_LOG_JSONL" ]]; then
  for status in finished deploy-success deploy-failed verify-failed skipped-already-implemented skipped-missing-cwd skipped-preflight-blocker skipped-dry-run skipped-no-deploy-command cancelled error startup-failed; do
    n=$(jq -c --arg s "$status" 'select(.status == $s)' "$RUN_LOG_JSONL" 2>/dev/null | wc -l | tr -d ' ')
    printf "  %-32s %d\n" "$status:" "$n"
  done
fi

err_n=$(jq -c 'select(.status == "error" or .status == "deploy-failed" or .status == "verify-failed")' "$RUN_LOG_JSONL" 2>/dev/null | wc -l | tr -d ' ')
sf_n=$(jq -c 'select(.status == "startup-failed")' "$RUN_LOG_JSONL" 2>/dev/null | wc -l | tr -d ' ')
if [[ "$sf_n" -gt 0 ]]; then exit 1; fi
if [[ "$err_n" -gt 0 ]]; then exit 2; fi
exit 0
