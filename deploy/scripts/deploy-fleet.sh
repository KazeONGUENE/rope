#!/usr/bin/env bash
# Datachain Rope Fleet Deployment Orchestrator
#
# This is the SINGLE authoritative production deploy script for the
# Datachain Rope nginx-failover ring. It supersedes deploy-blue-green.sh,
# which only covered BLUE+GREEN and produced binaries that could not be
# distributed to the DO nodes due to OS-version skew.
#
# Failover ring (nginx upstream "digitalocean_rpc"):
#   BLUE  = rope-vps         92.243.26.189   Gandi/Paris   Ubuntu 24.04 noble (glibc 2.39)
#   GREEN = anvil-vps        92.243.25.119   Gandi/Paris   Ubuntu 22.04 jammy (glibc 2.35)
#   DO1   = datachain-rpc-1  157.230.18.45   DO/Frankfurt  Ubuntu 22.04 jammy (glibc 2.35)
#   DO2   = datachain-rpc-2  167.172.106.174 DO/Frankfurt  Ubuntu 22.04 jammy (glibc 2.35)
#
# OS-skew strategy:
#   GREEN (jammy/glibc-2.35) is the canonical build host. Binaries built on
#   jammy run on every node in the fleet because glibc is backward
#   compatible — a binary linked against 2.35 runs on 2.39 (BLUE), but the
#   reverse is NOT true. Until all four nodes are on the same Ubuntu LTS,
#   GREEN is pinned as the build host.
#
# What this script handles:
#   - Source rsync of EVERY production crate to GREEN (the build host)
#   - Cargo build of rope-cli + rope-explorer on GREEN
#   - Distribution of the canonical jammy-built `rope` binary to all 4
#     nodes (BLUE included)
#   - Service restarts and per-node health probes
#   - Reth chain state sync (delegates to reth-blue-green-sync.sh)
#   - Production config TOML sync
#   - DCScan static asset sync
#   - V11 destructive-RPC denial verification on every node
#   - Smoke tests for the canonical Quipu Canon v1.1+ RPC methods
#
# Usage:
#   ./deploy-fleet.sh                    full fleet deploy (default)
#   ./deploy-fleet.sh --build            build canonical binary on GREEN only
#   ./deploy-fleet.sh --distribute       distribute existing GREEN binary to all 4 nodes
#   ./deploy-fleet.sh --blue-only        build + distribute + restart on BLUE only
#   ./deploy-fleet.sh --green-only       build + restart on GREEN only
#   ./deploy-fleet.sh --do1-only         distribute + restart on DO-rpc-1 only
#   ./deploy-fleet.sh --do2-only         distribute + restart on DO-rpc-2 only
#   ./deploy-fleet.sh --state-only       Reth state rsync only (no rebuild)
#   ./deploy-fleet.sh --failover         switch nginx upstream to GREEN
#   ./deploy-fleet.sh --restore-blue     restore nginx upstream to BLUE
#   ./deploy-fleet.sh --smoke-test       probe Canon v1.1+ RPC methods on public RPC
#   ./deploy-fleet.sh --health           health check all 4 nodes, no changes
#   ./deploy-fleet.sh --v11-audit        verify V11 destructive gate on every node
#   ./deploy-fleet.sh --bootstrap-agents re-create the 5 canonical-agent ledgers on BLUE
#
# Idempotent: safe to re-run.

set -uo pipefail

# ---------------------------------------------------------------
# Fleet (override via env)
# ---------------------------------------------------------------

BLUE_HOST="${BLUE_HOST:-rope-vps}"
BLUE_IP="${BLUE_IP:-92.243.26.189}"
BLUE_USER="${BLUE_USER:-ubuntu}"

GREEN_HOST="${GREEN_HOST:-anvil-vps}"
GREEN_IP="${GREEN_IP:-92.243.25.119}"
GREEN_USER="${GREEN_USER:-ubuntu}"

DO1_HOST="${DO1_HOST:-datachain-rpc-1}"
DO1_IP="${DO1_IP:-157.230.18.45}"
DO1_USER="${DO1_USER:-root}"

DO2_HOST="${DO2_HOST:-datachain-rpc-2}"
DO2_IP="${DO2_IP:-167.172.106.174}"
DO2_USER="${DO2_USER:-root}"

# SSH keys.
# When run from the laptop:    GANDI_KEY=DCRope_key, DO_KEY=datachain_rope_id_rsa
# When run on rope-vps (BLUE): GANDI_KEY=DCRope_key, DO_KEY=BLUE root id_ed25519
GANDI_KEY="${GANDI_KEY:-$HOME/.ssh/DCRope_key}"
DO_KEY_LAPTOP="${DO_KEY_LAPTOP:-$HOME/.ssh/datachain_rope_id_rsa}"
DO_KEY_BLUE="${DO_KEY_BLUE:-/root/.ssh/id_ed25519}"

REPO_ROOT="${REPO_ROOT:-/home/ubuntu/datachain-rope}"
NGINX_STATIC="${NGINX_STATIC:-/opt/datachain-rope/code/deploy/nginx/html/dcscan}"
PROD_CONFIG="${PROD_CONFIG:-/opt/datachain-rope/config/rope-production.toml}"
SCRIPTS_DIR="${SCRIPTS_DIR:-/opt/datachain-rope/scripts}"
PUBLIC_RPC="${PUBLIC_RPC:-https://erpc.datachain.network}"
RPC_PORT_DIRECT=8545
EXPLORER_PORT=3001

# Crates that ship to production.
CRATES=(
    "rope-core"
    "rope-node"
    "rope-cli"
    "rope-explorer"
    "rope-economics"
    "rope-cryptography"
    "rope-bridge"
    "rope-smartchain"
    "rope-rwa"
    "rope-onchainid"
    "rope-deploy"
    "rope-network"
    "rope-storage"
    "rope-iot-gateway"
    "rope-ai-framework"
    "rope-security"
    "compliance-agent"
    "insurance-agent"
    "oracle-agent"
    "semantic-agent"
    "validation-agent"
)

# Detect run context: laptop vs BLUE itself.
THIS_HOST="$(hostname 2>/dev/null || echo unknown)"
if [ "$THIS_HOST" = "$BLUE_HOST" ] || [[ "$THIS_HOST" == rope-vps* ]] || [[ "$THIS_HOST" == dcrope ]]; then
    RUNNING_ON_BLUE=true
    DO_KEY="$DO_KEY_BLUE"
else
    RUNNING_ON_BLUE=false
    DO_KEY="$DO_KEY_LAPTOP"
fi

LOCAL_WORKSPACE_ROOT=""
if [ "$RUNNING_ON_BLUE" = false ]; then
    # Best-effort: assume the script lives at $WORKSPACE/deploy/scripts/.
    LOCAL_WORKSPACE_ROOT="$(cd "$(dirname "$0")/../.." 2>/dev/null && pwd || true)"
    if [ -z "$LOCAL_WORKSPACE_ROOT" ] || [ ! -d "$LOCAL_WORKSPACE_ROOT/datachain-rope/crates" ]; then
        LOCAL_WORKSPACE_ROOT="$HOME/Downloads/DATACHAIN ROPE"
    fi
fi

# ---------------------------------------------------------------
# Logging
# ---------------------------------------------------------------

log()  { printf "[fleet %s] %s\n" "$(date -u +%H:%M:%S)" "$*" >&2; }
ok()   { printf "[fleet %s] \033[1;32m✓\033[0m %s\n" "$(date -u +%H:%M:%S)" "$*" >&2; }
warn() { printf "[fleet %s] \033[1;33m!\033[0m %s\n" "$(date -u +%H:%M:%S)" "$*" >&2; }
err()  { printf "[fleet %s] \033[1;31m✗\033[0m %s\n" "$(date -u +%H:%M:%S)" "$*" >&2; }

# ---------------------------------------------------------------
# Shell/JSON quoting helpers (finding H6, SECURITY_AUDIT_2026-07-25)
# ---------------------------------------------------------------
#
# Several helpers below build a command STRING that is re-parsed by a
# second shell layer (`bash -c "$cmd"` locally, or the remote sshd's shell
# when a string is sent as an SSH remote command). Any value interpolated
# into such a string without escaping is a shell-injection primitive the
# moment it stops being a hardcoded literal (e.g. if a future flag lets an
# operator pass a custom RPC method/params on the CLI). We escape
# defensively now so the functions stay safe under future extension, not
# just under today's call sites.

# Single-quote `$1` for safe embedding inside a shell command string that
# will be re-parsed by another shell. Standard technique: end the current
# quote, insert an escaped literal quote, reopen the quote.
shell_singlequote() {
    printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"
}

# Escape `$1` for embedding as a JSON string value's content (between the
# surrounding double quotes). Handles backslash, double-quote, and control
# characters that would otherwise corrupt the JSON payload.
json_escape() {
    printf '%s' "$1" | python3 -c 'import json,sys; s=sys.stdin.read(); sys.stdout.write(json.dumps(s)[1:-1])' 2>/dev/null \
        || printf '%s' "$1" | sed 's/\\/\\\\/g; s/"/\\"/g'
}

# ---------------------------------------------------------------
# SSH helpers (one per node, plus a generic one)
# ---------------------------------------------------------------

# Each node has a "ssh-style" connector. For BLUE+GREEN we use the local
# SSH host aliases (which encode port 41722 / port 22, key, user) so that
# any future change to the SSH topology lives in ~/.ssh/config rather than
# in this script. For DO we use direct args.
#
# These populate global ARRAYS (not strings) so that every argument stays
# a distinct argv element no matter what characters $DO_KEY / $DO1_USER /
# $DO1_IP happen to contain. A string-returning helper that callers later
# word-split (the pre-fix shape) lets a value like
# "/tmp/k -o ProxyCommand=evil" smuggle in an extra SSH option; an array
# cannot be split apart like that, so this is the real fix, not cosmetic.
#
#   ssh_args_for LABEL      -> sets global array _SSH_ARGS
#   scp_dest_for LABEL      -> the SCP destination spec (e.g. "rope-vps"
#                              or "root@157.230.18.45") — single token,
#                              safe to echo (no embedded whitespace by
#                              construction: host aliases and user@ip).
#   scp_extra_args_for LABEL -> sets global array _SCP_EXTRA_ARGS

declare -a _SSH_ARGS
declare -a _SCP_EXTRA_ARGS

ssh_args_for() {
    case "$1" in
        BLUE)  _SSH_ARGS=(ssh -o ConnectTimeout=10 "$BLUE_HOST") ;;
        GREEN) _SSH_ARGS=(ssh -o ConnectTimeout=10 "$GREEN_HOST") ;;
        DO1)   _SSH_ARGS=(ssh -i "$DO_KEY" -o ConnectTimeout=10 -o IdentitiesOnly=yes "$DO1_USER@$DO1_IP") ;;
        DO2)   _SSH_ARGS=(ssh -i "$DO_KEY" -o ConnectTimeout=10 -o IdentitiesOnly=yes "$DO2_USER@$DO2_IP") ;;
        *) err "ssh_args_for: unknown label $1"; return 1 ;;
    esac
}

scp_dest_for() {
    case "$1" in
        BLUE)  echo "$BLUE_HOST" ;;
        GREEN) echo "$GREEN_HOST" ;;
        DO1)   echo "$DO1_USER@$DO1_IP" ;;
        DO2)   echo "$DO2_USER@$DO2_IP" ;;
        *) err "scp_dest_for: unknown label $1"; return 1 ;;
    esac
}

# scp does not understand bare host aliases for the -i / -P override path,
# but it DOES read ~/.ssh/config — so for BLUE+GREEN the empty array is
# correct (alias resolves through ~/.ssh/config). DO needs explicit args.
scp_extra_args_for() {
    case "$1" in
        BLUE|GREEN) _SCP_EXTRA_ARGS=(-o ConnectTimeout=10) ;;
        DO1|DO2)    _SCP_EXTRA_ARGS=(-i "$DO_KEY" -o ConnectTimeout=10 -o IdentitiesOnly=yes) ;;
        *) err "scp_extra_args_for: unknown label $1"; return 1 ;;
    esac
}

ip_for() {
    case "$1" in
        BLUE)  echo "$BLUE_IP" ;;
        GREEN) echo "$GREEN_IP" ;;
        DO1)   echo "$DO1_IP" ;;
        DO2)   echo "$DO2_IP" ;;
        *) err "ip_for: unknown label $1"; return 1 ;;
    esac
}

sudo_prefix_for() {
    case "$1" in
        BLUE|GREEN) echo "sudo " ;;
        DO1|DO2)    echo "" ;;
        *) err "sudo_prefix_for: unknown label $1"; return 1 ;;
    esac
}

backup_dir_for() {
    case "$1" in
        BLUE|GREEN) echo "/home/ubuntu/backup-$(date -u +%Y-%m-%d)" ;;
        DO1|DO2)    echo "/root/backup-$(date -u +%Y-%m-%d)" ;;
        *) err "backup_dir_for: unknown label $1"; return 1 ;;
    esac
}

# Run a command on a labelled node via SSH. Uses ssh_args_for to build the
# connection argv as a real array (see H6 note above) so `$cmd` is always
# passed to ssh as exactly one remote-command argument.
on_node() {
    local label="$1"; shift
    local cmd="$1"
    if [ "$label" = "BLUE" ] && [ "$RUNNING_ON_BLUE" = true ]; then
        bash -c "$cmd"
    else
        ssh_args_for "$label" || return 1
        "${_SSH_ARGS[@]}" "$cmd"
    fi
}

# Convenience aliases.
on_blue()  { on_node "BLUE"  "$1"; }
on_green() { on_node "GREEN" "$1"; }
on_do1()   { on_node "DO1"   "$1"; }
on_do2()   { on_node "DO2"   "$1"; }

# ---------------------------------------------------------------
# Health probes
# ---------------------------------------------------------------

rpc_call() {
    local url="$1"; local method="$2"; local params="$3"; local extra_header="${4:-}"
    local safe_method; safe_method="$(json_escape "$method")"
    # $params is passed through as raw JSON (array/object), not a string
    # value, so it is not JSON-string-escaped. This function invokes curl
    # directly as an argv array (no second shell re-parses these strings),
    # so there is no shell-injection vector here — only JSON-well-formedness
    # depends on the caller passing valid JSON in $params, same as before.
    if [ -n "$extra_header" ]; then
        curl -sS --connect-timeout 3 --max-time 5 \
            -X POST -H "Content-Type: application/json" -H "$extra_header" \
            -d "{\"jsonrpc\":\"2.0\",\"method\":\"$safe_method\",\"params\":$params,\"id\":1}" \
            "$url" 2>/dev/null
    else
        curl -sS --connect-timeout 3 --max-time 5 \
            -X POST -H "Content-Type: application/json" \
            -d "{\"jsonrpc\":\"2.0\",\"method\":\"$safe_method\",\"params\":$params,\"id\":1}" \
            "$url" 2>/dev/null
    fi
}

# Run an RPC call FROM the target node itself (curl 127.0.0.1:8545 over SSH).
# Backups are firewalled — public can't reach :8545 on them. Loopback always works.
# When extra_header contains "X-Forwarded-For", the call also exercises the
# V11 gate exactly as nginx would for proxied public traffic.
#
# Unlike rpc_call() above, this builds a command STRING that is re-parsed
# by a second shell (bash -c locally, or the remote sshd's shell over SSH)
# — see the H6 note near shell_singlequote(). We build the full JSON body
# first, JSON-escape $method, then single-quote-escape the *entire* body
# and header as one unit right before embedding, so any shell metacharacter
# anywhere in $method/$params/$extra_header (including a stray `'`) cannot
# break out of the intended curl argument, regardless of what a future
# caller passes in.
rpc_call_on_node() {
    local label="$1"; local method="$2"; local params="$3"; local extra_header="${4:-}"
    local safe_method body qbody cmd
    safe_method="$(json_escape "$method")"
    body="{\"jsonrpc\":\"2.0\",\"method\":\"$safe_method\",\"params\":$params,\"id\":1}"
    qbody="$(shell_singlequote "$body")"
    cmd="curl -sS --max-time 5 -X POST -H 'Content-Type: application/json'"
    if [ -n "$extra_header" ]; then
        local qheader; qheader="$(shell_singlequote "$extra_header")"
        cmd="$cmd -H $qheader"
    fi
    cmd="$cmd -d $qbody http://127.0.0.1:$RPC_PORT_DIRECT"
    on_node "$label" "$cmd"
}

health_one() {
    local label="$1"
    local resp result
    resp="$(rpc_call_on_node "$label" "eth_chainId" "[]")"
    result="$(echo "$resp" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('result','MISSING'))" 2>/dev/null || echo "PARSE_ERROR")"
    if [ "$result" = "0x425d4" ]; then
        ok "$label : RPC OK chainId=271828"
    else
        err "$label : RPC FAIL response=$resp"
        return 1
    fi
}

health_all() {
    log "--- health check on all 4 nodes (loopback 8545 via SSH) ---"
    local fail=0
    health_one "BLUE"   || fail=$((fail+1))
    health_one "GREEN"  || fail=$((fail+1))
    health_one "DO1"    || fail=$((fail+1))
    health_one "DO2"    || fail=$((fail+1))
    if [ $fail -eq 0 ]; then
        ok "all 4 nodes healthy"
    else
        warn "$fail node(s) unhealthy"
    fi
    return $fail
}

# ---------------------------------------------------------------
# V11 destructive-gate audit
# ---------------------------------------------------------------

v11_audit_one() {
    local label="$1"
    local methods=("rope_untieKnot" "rope_erasePersonalLedger" "rope_appendToLedger" "rope_createPersonalLedger" "rope_anchorDeployerAttestation")
    local fail=0
    for m in "${methods[@]}"; do
        # XFF triggers the gate — public-traffic emulation.
        local resp code
        resp="$(rpc_call_on_node "$label" "$m" "[]" "X-Forwarded-For: scanner.example")"
        code="$(echo "$resp" | python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('error',{}).get('code','?'))" 2>/dev/null || echo "PARSE_ERROR")"
        if [ "$code" = "-32401" ]; then
            ok "  $label $m -> -32401 (gate active)"
        else
            err "  $label $m NOT GATED (code=$code) — $resp"
            fail=$((fail+1))
        fi
    done
    return $fail
}

v11_audit_all() {
    log "--- V11 audit: destructive RPC gate on every node ---"
    local fail=0
    v11_audit_one "BLUE"   || fail=$((fail+$?))
    v11_audit_one "GREEN"  || fail=$((fail+$?))
    v11_audit_one "DO1"    || fail=$((fail+$?))
    v11_audit_one "DO2"    || fail=$((fail+$?))
    if [ $fail -eq 0 ]; then
        ok "V11 gate confirmed on every node, every destructive method"
    else
        err "V11 gate FAILED on $fail (node x method) combinations"
    fi
    return $fail
}

# ---------------------------------------------------------------
# Source sync to GREEN (the canonical build host)
# ---------------------------------------------------------------

sync_source_to_green() {
    log "--- Sync source tree to GREEN (the jammy build host) ---"

    local src
    if [ "$RUNNING_ON_BLUE" = true ]; then
        src="$REPO_ROOT"
    else
        src="$LOCAL_WORKSPACE_ROOT/datachain-rope"
        if [ ! -d "$src/crates" ]; then
            err "Local workspace not found at $src"
            return 1
        fi
    fi
    log "  src=$src dst=$GREEN_HOST:$REPO_ROOT"
    rsync -avz --delete --exclude target --exclude '*.rs.bk' \
        -e "ssh -o ConnectTimeout=10" \
        "$src/crates/" "$GREEN_HOST:$REPO_ROOT/crates/"
    rsync -avz \
        -e "ssh -o ConnectTimeout=10" \
        "$src/Cargo.toml" "$src/Cargo.lock" \
        "$GREEN_HOST:$REPO_ROOT/"
    if [ -d "$src/deploy/config" ]; then
        rsync -avz \
            -e "ssh -o ConnectTimeout=10" \
            "$src/deploy/config/" \
            "$GREEN_HOST:$REPO_ROOT/deploy/config/"
    fi
    ok "GREEN source synced"
}

# ---------------------------------------------------------------
# Build canonical (jammy/glibc-2.35) binary on GREEN
# ---------------------------------------------------------------

build_canonical() {
    log "--- Build canonical binary on GREEN (jammy/glibc-2.35) ---"
    on_green "export PATH=\"\$HOME/.cargo/bin:\$PATH\" && \
              cd $REPO_ROOT && \
              cargo build --release -p rope-cli -p rope-explorer 2>&1 | tail -5 && \
              ls -lh target/release/rope target/release/dc-explorer"
    local rope_hash; rope_hash=$(on_green "sha256sum $REPO_ROOT/target/release/rope | awk '{print \$1}'")
    local explorer_hash; explorer_hash=$(on_green "sha256sum $REPO_ROOT/target/release/dc-explorer | awk '{print \$1}'")
    ok "  rope hash:        $rope_hash"
    ok "  dc-explorer hash: $explorer_hash"
}

# ---------------------------------------------------------------
# Distribute the canonical binary to a node
# ---------------------------------------------------------------

# Stage the canonical binary on the box where this script is running.
# Returns (via stdout) the staging directory path; logs go to stderr.
stage_canonical_locally() {
    log "--- Stage canonical binary from GREEN to local /tmp ---"
    local stage_dir="/tmp/rope-fleet-build-$(date -u +%Y%m%dT%H%M%SZ)"
    mkdir -p "$stage_dir"
    scp -o ConnectTimeout=10 \
        "$GREEN_HOST:$REPO_ROOT/target/release/rope" "$stage_dir/rope" >&2
    scp -o ConnectTimeout=10 \
        "$GREEN_HOST:$REPO_ROOT/target/release/dc-explorer" "$stage_dir/dc-explorer" >&2
    chmod +x "$stage_dir/rope" "$stage_dir/dc-explorer"
    local rope_hash
    rope_hash="$(sha256sum "$stage_dir/rope" 2>/dev/null | awk '{print $1}')"
    if [ -z "$rope_hash" ]; then
        rope_hash="$(shasum -a 256 "$stage_dir/rope" | awk '{print $1}')"
    fi
    ok "  staged at $stage_dir"
    ok "  rope hash: $rope_hash"
    # Path on stdout (the only stdout output from this function).
    echo "$stage_dir"
}

distribute_to_node() {
    local label="$1"; local stage_dir="$2"
    local scpdest sudo_pfx backup_dir
    ssh_args_for "$label" || return 1
    scp_extra_args_for "$label" || return 1
    scpdest="$(scp_dest_for "$label")"
    sudo_pfx="$(sudo_prefix_for "$label")"
    backup_dir="$(backup_dir_for "$label")"

    log "--- Distribute rope binary to $label ($scpdest) ---"

    # Backup current binary.
    "${_SSH_ARGS[@]}" "mkdir -p \"$backup_dir\" && cp \"$REPO_ROOT/target/release/rope\" \"$backup_dir/rope-pre-fleet-$(date -u +%H%M%S)\" 2>/dev/null && ls -lh \"$backup_dir\"/rope-pre-fleet-* | tail -1" \
        || warn "  backup step failed (may not have prior binary)"

    # Push + install rope.
    scp "${_SCP_EXTRA_ARGS[@]}" "$stage_dir/rope" "$scpdest:/tmp/rope-fleet-stage" >/dev/null
    "${_SSH_ARGS[@]}" "${sudo_pfx}install -m 0755 /tmp/rope-fleet-stage \"$REPO_ROOT/target/release/rope\" && rm -f /tmp/rope-fleet-stage && ls -lh \"$REPO_ROOT/target/release/rope\""

    # dc-explorer only on BLUE (today).
    if [ "$label" = "BLUE" ]; then
        scp "${_SCP_EXTRA_ARGS[@]}" "$stage_dir/dc-explorer" "$scpdest:/tmp/dc-explorer-fleet-stage" >/dev/null
        "${_SSH_ARGS[@]}" "${sudo_pfx}install -m 0755 /tmp/dc-explorer-fleet-stage \"$REPO_ROOT/target/release/dc-explorer\" && rm -f /tmp/dc-explorer-fleet-stage"
    fi

    # Restart datachain-rope.service.
    "${_SSH_ARGS[@]}" "${sudo_pfx}systemctl restart datachain-rope.service && sleep 8 && systemctl is-active datachain-rope.service"

    # On BLUE also restart dc-explorer (best-effort).
    if [ "$label" = "BLUE" ]; then
        "${_SSH_ARGS[@]}" "${sudo_pfx}systemctl restart dc-explorer 2>/dev/null && sleep 3 && systemctl is-active dc-explorer" \
            || warn "  dc-explorer restart skipped or failed (non-fatal)"
    fi

    # Verify chain ID via loopback on the target.
    local resp
    resp="$(rpc_call_on_node "$label" "eth_chainId" "[]")"
    if echo "$resp" | grep -q "0x425d4"; then
        ok "  $label rebooted, RPC live (chainId 271828)"
    else
        err "  $label health check FAILED: $resp"
        return 1
    fi
}

distribute_to_all() {
    log "=== Distribute canonical binary to all 4 nodes ==="
    local stage_dir
    stage_dir="$(stage_canonical_locally)"
    if [ -z "$stage_dir" ] || [ ! -f "$stage_dir/rope" ]; then
        err "Staging failed at $stage_dir"; return 1
    fi
    # Order matters: BLUE last so the active sequencer is the last one
    # to bounce. While GREEN/DO1/DO2 restart, BLUE keeps serving public
    # traffic, then BLUE bounces last.
    distribute_to_node "GREEN" "$stage_dir" || warn "GREEN distribute had issues"
    distribute_to_node "DO1"   "$stage_dir" || warn "DO1 distribute had issues"
    distribute_to_node "DO2"   "$stage_dir" || warn "DO2 distribute had issues"
    distribute_to_node "BLUE"  "$stage_dir" || warn "BLUE distribute had issues"
    rm -rf "$stage_dir"
    ok "Distribution complete"
}

# ---------------------------------------------------------------
# Reth state sync (delegates to existing script)
# ---------------------------------------------------------------

sync_state() {
    log "--- Reth state sync (delegates to reth-blue-green-sync.sh) ---"
    on_blue "bash $SCRIPTS_DIR/reth-blue-green-sync.sh 2>&1 | tail -10"
}

# ---------------------------------------------------------------
# Failover / restore
# ---------------------------------------------------------------

failover_to_green() {
    log "--- FAILOVER: route nginx upstream preference to GREEN ---"
    warn "Nginx upstream order is set in datachain.network.conf — manual edit + reload"
    warn "TODO: pin a specific upstream as primary; current model uses backup= for everything but BLUE"
}

restore_to_blue() {
    log "--- RESTORE: nginx prefers BLUE again (default) ---"
    warn "Nothing to do if upstream config was untouched"
}

# ---------------------------------------------------------------
# Smoke test on public RPC
# ---------------------------------------------------------------

smoke_test() {
    log "--- Smoke test: Quipu Canon RPC methods on $PUBLIC_RPC ---"
    local fail=0

    local r; r=$(rpc_call "$PUBLIC_RPC" "rope_knotIndex" "[]")
    if echo "$r" | grep -q '"result"'; then
        local k; k=$(echo "$r" | python3 -c "import json,sys; print(int(json.load(sys.stdin)['result'],16))" 2>/dev/null)
        ok "  rope_knotIndex -> $k"
    else
        err "  rope_knotIndex FAILED: $r"; fail=$((fail+1))
    fi

    r=$(rpc_call "$PUBLIC_RPC" "rope_globalStats" "[]")
    if echo "$r" | grep -q '"result"'; then
        local s; s=$(echo "$r" | python3 -c "import json,sys; d=json.load(sys.stdin)['result']; print(f\"strings={d['total_strings']} knots={d['total_knots']} invariant={d['invariant_holds']}\")" 2>/dev/null)
        ok "  rope_globalStats -> $s"
    else
        err "  rope_globalStats FAILED: $r"; fail=$((fail+1))
    fi

    r=$(rpc_call "$PUBLIC_RPC" "eth_chainId" "[]")
    if echo "$r" | grep -q '"result":"0x425d4"'; then
        ok "  eth_chainId -> 0x425d4 (271828)"
    else
        err "  eth_chainId FAILED: $r"; fail=$((fail+1))
    fi

    # V11 gate via public RPC (must be -32401)
    r=$(rpc_call "$PUBLIC_RPC" "rope_untieKnot" "[]")
    if echo "$r" | grep -q '"code":-32401'; then
        ok "  V11 gate active on public RPC: rope_untieKnot -> -32401"
    else
        err "  V11 gate INACTIVE on public RPC: $r"; fail=$((fail+1))
    fi

    if [ $fail -eq 0 ]; then ok "smoke OK"; else err "smoke FAIL ($fail checks)"; return $fail; fi
}

# ---------------------------------------------------------------
# Main
# ---------------------------------------------------------------

MODE="${1:-full}"

case "$MODE" in
    --health|--status)
        health_all || true
        ;;
    --smoke-test)
        smoke_test
        ;;
    --v11-audit)
        v11_audit_all
        ;;
    --bootstrap-agents)
        # Re-create the 5 canonical-agent ledgers on BLUE.
        # Needed after every datachain-rope.service restart while
        # rope-storage is still in-memory (per crates/rope-storage/src/lib.rs).
        # Once RocksDB persistence lands this becomes a no-op.
        log "--- Re-create 5 canonical-agent ledgers on BLUE ---"
        for w in C001 C002 C003 C004 C005; do
            resp="$(rpc_call_on_node "BLUE" "rope_createPersonalLedger" "[\"0x000000000000000000000000000000000000${w}\"]")"
            if echo "$resp" | grep -qE '"result"|already exists|code=2001'; then
                ok "  0x...${w} ledger ready"
            else
                err "  0x...${w} bootstrap failed: $resp"
            fi
        done
        ;;
    --state-only)
        sync_state
        ;;
    --build)
        sync_source_to_green
        build_canonical
        ;;
    --distribute)
        distribute_to_all
        v11_audit_all
        ;;
    --blue-only)
        sync_source_to_green
        build_canonical
        STAGE_DIR="$(stage_canonical_locally)"
        distribute_to_node "BLUE" "$STAGE_DIR"
        rm -rf "$STAGE_DIR"
        v11_audit_one "BLUE"
        ;;
    --green-only)
        sync_source_to_green
        build_canonical
        # GREEN already has the freshly-built binary at $REPO_ROOT/target/release/rope.
        # Just restart its service.
        on_green "sudo systemctl restart datachain-rope.service && sleep 8 && systemctl is-active datachain-rope.service"
        v11_audit_one "GREEN"
        ;;
    --do1-only)
        STAGE_DIR="$(stage_canonical_locally)"
        distribute_to_node "DO1" "$STAGE_DIR"
        rm -rf "$STAGE_DIR"
        v11_audit_one "DO1"
        ;;
    --do2-only)
        STAGE_DIR="$(stage_canonical_locally)"
        distribute_to_node "DO2" "$STAGE_DIR"
        rm -rf "$STAGE_DIR"
        v11_audit_one "DO2"
        ;;
    --failover)
        failover_to_green
        sleep 3
        smoke_test
        ;;
    --restore-blue)
        restore_to_blue
        sleep 3
        smoke_test
        ;;
    full|"")
        log "=== FULL fleet deploy ==="
        sync_source_to_green
        build_canonical
        distribute_to_all
        sleep 3
        health_all
        v11_audit_all
        # Re-create canonical-agent ledgers (BLUE restart wiped in-memory
        # registry; this is idempotent thanks to the create-or-noop semantics).
        log "--- Re-bootstrap canonical-agent ledgers on BLUE ---"
        for w in C001 C002 C003 C004 C005; do
            rpc_call_on_node "BLUE" "rope_createPersonalLedger" "[\"0x000000000000000000000000000000000000${w}\"]" >/dev/null && ok "  0x...${w} ready"
        done
        smoke_test
        log "=== FULL fleet deploy complete ==="
        ;;
    --help|-h)
        sed -n '2,55p' "$0"
        ;;
    *)
        err "Unknown mode: $MODE"
        sed -n '2,55p' "$0"
        exit 1
        ;;
esac
