#!/usr/bin/env bash
# Regression tests for deploy/scripts/nginx-config-audit.py.
#
# Every fixture below exercises exactly one failure mode the audit is
# meant to catch. If the audit ever regresses (e.g. someone loosens
# `role=write-primary` to accept a backup server), the corresponding
# fixture test flips from PASS to FAIL and CI stays red.
#
# Runs stdlib-only Python 3.8+. No pytest, no bash 4 features.
#
#   Usage:
#       deploy/scripts/tests/test-nginx-config-audit.sh
#       # OR from the repo root:
#       bash deploy/scripts/tests/test-nginx-config-audit.sh
#
# Exit 0 = all fixtures behaved as expected, 1 = at least one drift.

set -u

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"
AUDIT="$REPO_ROOT/deploy/scripts/nginx-config-audit.py"

if [[ ! -x "$AUDIT" ]]; then
    echo "FATAL: audit script not found or not executable: $AUDIT" >&2
    exit 2
fi

PASSES=0
FAILURES=0
FAIL_DETAIL=()

# _assert_exit <expected_rc> <fixture_name> <expected_stderr_substring> <fixture_content>
_assert_exit() {
    local expected_rc="$1"
    local name="$2"
    local expect="$3"
    local body="$4"

    local tmp
    tmp="$(mktemp -t "nginx-audit-fixture.XXXXXX.conf")"
    printf '%s\n' "$body" > "$tmp"

    local actual_rc actual_out actual_err
    actual_out="$(python3 "$AUDIT" "$tmp" 2>/tmp/nginx-audit-stderr.$$)"
    actual_rc=$?
    actual_err="$(cat /tmp/nginx-audit-stderr.$$)"
    rm -f "$tmp" /tmp/nginx-audit-stderr.$$

    local combined="$actual_out"$'\n'"$actual_err"

    if [[ "$actual_rc" != "$expected_rc" ]]; then
        FAILURES=$((FAILURES + 1))
        FAIL_DETAIL+=("$name: expected exit=$expected_rc, got exit=$actual_rc. Output: $combined")
        printf "  FAIL  %s (exit=%s expected %s)\n" "$name" "$actual_rc" "$expected_rc"
        return
    fi
    if [[ -n "$expect" && "$combined" != *"$expect"* ]]; then
        FAILURES=$((FAILURES + 1))
        FAIL_DETAIL+=("$name: expected stderr to contain '$expect'. Got: $combined")
        printf "  FAIL  %s (missing substring '%s')\n" "$name" "$expect"
        return
    fi
    PASSES=$((PASSES + 1))
    printf "  ok    %s\n" "$name"
}

echo "==> Fixture tests"

# ---------- F01: write-primary MUST NOT have a backup server -----------------
_assert_exit 1 "F01-write-primary-with-backup" \
    "role=write-primary MUST NOT declare any \`backup\` server" \
    "$(cat <<'EOF'
# nginx-audit: role=write-primary port=8545 write-safe=true must-include=host.docker.internal:8545
upstream f01_bad {
    server host.docker.internal:8545;
    server 92.243.25.119:8545 backup;
}
EOF
)"

# ---------- F02: write-primary MUST target BLUE ------------------------------
_assert_exit 1 "F02-write-primary-wrong-primary" \
    "must target BLUE" \
    "$(cat <<'EOF'
# nginx-audit: role=write-primary port=8545 write-safe=true
upstream f02_bad {
    server 157.230.18.45:8545;
}
EOF
)"

# ---------- F03: read-failover MUST NOT collapse to 1 server -----------------
# THIS IS THE 2026-08-23 `digitalocean_rpc` bug. If this test ever flips
# to PASS, the audit no longer catches the class of regression that made
# 3 of the 4 Chainlist endpoints go red.
_assert_exit 1 "F03-read-failover-collapsed-to-one" \
    "requires >=2 servers" \
    "$(cat <<'EOF'
# nginx-audit: role=read-failover min-servers=4 port=8545
upstream f03_regressed_digitalocean_rpc {
    server host.docker.internal:8545;
}
EOF
)"

# ---------- F04: read-failover on wrong port ---------------------------------
_assert_exit 1 "F04-read-failover-wrong-port" \
    "is not on port 8545" \
    "$(cat <<'EOF'
# nginx-audit: role=read-failover min-servers=2 port=8545
upstream f04_bad {
    server host.docker.internal:8546;
    server 92.243.25.119:8545 backup;
}
EOF
)"

# ---------- F05: read-failover missing a must-include node -------------------
_assert_exit 1 "F05-read-failover-missing-required" \
    "missing required server '157.230.18.45:8545'" \
    "$(cat <<'EOF'
# nginx-audit: role=read-failover min-servers=2 port=8545 must-include=host.docker.internal:8545,157.230.18.45:8545
upstream f05_bad {
    server host.docker.internal:8545;
    server 92.243.25.119:8545 backup;
}
EOF
)"

# ---------- F06: attesters-only MUST NOT include BLUE ------------------------
# THIS IS THE 2026-08-14 /v1/read ghost-tx hazard. If this test flips,
# a raw tx sent to /v1/read that hit BLUE would be silently accepted.
_assert_exit 1 "F06-attesters-only-includes-blue" \
    "role=read-attesters-only forbids BLUE" \
    "$(cat <<'EOF'
# nginx-audit: role=read-attesters-only min-servers=1 port=8545 must-exclude=host.docker.internal:8545
upstream f06_bad {
    server host.docker.internal:8545;
    server 92.243.25.119:8545;
}
EOF
)"

# ---------- F07: ws pool on HTTP port ----------------------------------------
# THIS IS THE 2026-08-11 ws.rope.network bug (proxy_pass to :8545 for a
# WS vhost). If this test flips, HTTP JSON-RPC clients would open a
# connection that never returns 101 Switching Protocols.
_assert_exit 1 "F07-ws-pool-on-http-port" \
    "is not on WS port 8546" \
    "$(cat <<'EOF'
# nginx-audit: role=ws-writer-only port=8546
upstream f07_bad {
    server host.docker.internal:8545;
}
EOF
)"

# ---------- F08: ws-failover with 1 server -----------------------------------
_assert_exit 1 "F08-ws-failover-single-server" \
    "role=ws-failover requires >=2 servers" \
    "$(cat <<'EOF'
# nginx-audit: role=ws-failover port=8546
upstream f08_bad {
    server host.docker.internal:8546;
}
EOF
)"

# ---------- F09: strict mode: unannotated upstream fails ---------------------
_assert_exit 1 "F09-unannotated-upstream" \
    "no \`# nginx-audit:\` annotation" \
    "$(cat <<'EOF'
upstream f09_bad {
    server host.docker.internal:8545;
}
EOF
)"

# ---------- F10: unknown role fails ------------------------------------------
_assert_exit 1 "F10-unknown-role" \
    "unknown role='bogus'" \
    "$(cat <<'EOF'
# nginx-audit: role=bogus min-servers=1
upstream f10_bad {
    server host.docker.internal:8545;
}
EOF
)"

# ---------- F11: valid read-failover passes ----------------------------------
_assert_exit 0 "F11-valid-read-failover" "" \
    "$(cat <<'EOF'
# nginx-audit: role=read-failover min-servers=4 port=8545 must-include=host.docker.internal:8545,92.243.25.119:8545,157.230.18.45:8545,167.172.106.174:8545
upstream f11_ok {
    server host.docker.internal:8545 max_fails=3 fail_timeout=5s;
    server 92.243.25.119:8545 backup;
    server 157.230.18.45:8545 backup;
    server 167.172.106.174:8545 backup;
}
EOF
)"

# ---------- F12: valid write-primary passes ----------------------------------
_assert_exit 0 "F12-valid-write-primary" "" \
    "$(cat <<'EOF'
# nginx-audit: role=write-primary port=8545 write-safe=true must-include=host.docker.internal:8545
upstream f12_ok {
    server host.docker.internal:8545;
}
EOF
)"

# ---------- F13: valid attesters-only passes ---------------------------------
_assert_exit 0 "F13-valid-attesters-only" "" \
    "$(cat <<'EOF'
# nginx-audit: role=read-attesters-only min-servers=3 port=8545 must-exclude=host.docker.internal:8545,127.0.0.1:8545
upstream f13_ok {
    server 92.243.25.119:8545 max_fails=2 fail_timeout=5s;
    server 157.230.18.45:8545 max_fails=2 fail_timeout=5s;
    server 167.172.106.174:8545 max_fails=2 fail_timeout=5s;
}
EOF
)"

# ---------- F14: malformed annotation is FATAL (exit 2) ----------------------
_assert_exit 2 "F14-fatal-malformed-min-servers" \
    "min-servers must be a positive integer" \
    "$(cat <<'EOF'
# nginx-audit: role=read-failover min-servers=abc
upstream f14_bad {
    server host.docker.internal:8545;
}
EOF
)"

# ---------- F15: annotation not adjacent to upstream is FATAL ----------------
_assert_exit 2 "F15-fatal-annotation-orphaned" \
    "not immediately above an \`upstream\` block" \
    "$(cat <<'EOF'
# nginx-audit: role=read-failover
server { listen 80; }
upstream f15_bad {
    server host.docker.internal:8545;
}
EOF
)"

# ---------- Live-conf sanity check ------------------------------------------
# The current annotated live conf MUST pass. If someone edits a real conf
# in a way that regresses, this backstop catches it in the same run.
echo
echo "==> Live-conf sanity check"
if python3 "$AUDIT" \
    "$REPO_ROOT/deploy/nginx/conf.d/datachain.network.conf" \
    "$REPO_ROOT/deploy/nginx/conf.d/rope.network.conf" \
    --quiet >/dev/null 2>&1; then
    printf "  ok    live-conf passes audit\n"
    PASSES=$((PASSES + 1))
else
    printf "  FAIL  live-conf FAILS audit - run \`python3 %s deploy/nginx/conf.d/*.conf\` to see why\n" \
        "$AUDIT"
    FAILURES=$((FAILURES + 1))
    FAIL_DETAIL+=("live-conf audit failed - see stdout above")
fi

echo
if [[ "$FAILURES" -gt 0 ]]; then
    echo "==> FAIL: $FAILURES failure(s), $PASSES pass(es)"
    for d in "${FAIL_DETAIL[@]}"; do
        echo " - $d"
    done
    exit 1
fi

echo "==> PASS: $PASSES/$((PASSES)) tests"
exit 0
