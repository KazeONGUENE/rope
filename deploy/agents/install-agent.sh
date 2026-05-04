#!/usr/bin/env bash
# install-agent.sh — install / upgrade a single Datachain Rope canonical
# AI agent on the local VPS. Idempotent: re-running is safe and only
# upgrades the systemd unit + restarts the service.
#
# Usage:
#   sudo deploy/agents/install-agent.sh oracle-agent
#   sudo deploy/agents/install-agent.sh validation-agent
#   sudo deploy/agents/install-agent.sh insurance-agent
#   sudo deploy/agents/install-agent.sh semantic-agent
#   sudo deploy/agents/install-agent.sh compliance-agent

set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: must run as root (try: sudo $0 $*)" >&2
    exit 1
fi

AGENT="${1:-}"
case "$AGENT" in
    oracle-agent|validation-agent|insurance-agent|semantic-agent|compliance-agent) ;;
    "")  echo "ERROR: missing agent name. Try one of: oracle-agent, validation-agent, insurance-agent, semantic-agent, compliance-agent" >&2; exit 1 ;;
    *)   echo "ERROR: unknown agent '$AGENT'" >&2; exit 1 ;;
esac

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DEPLOY_DIR="$REPO_ROOT/deploy/agents"
BIN="$REPO_ROOT/target/release/$AGENT"
USER_NAME="ubuntu"
GROUP_NAME="ubuntu"
ETC_DIR="/etc/$AGENT"
STATE_DIR="/var/lib/$AGENT"
SHARED_ETC="/etc/datachain-agents"

echo "=== Installing $AGENT from $REPO_ROOT ==="

# 1. Verify the binary exists. We deliberately do NOT build here — the
#    operator should have run `cargo build --release -p $AGENT` first
#    so the build is auditable and reproducible from a clean shell.
if [ ! -x "$BIN" ]; then
    cat <<EOF >&2
ERROR: $BIN does not exist or is not executable.

Build it first as the ubuntu user:

    cd $REPO_ROOT
    cargo build --release -p $AGENT

Then re-run this script.
EOF
    exit 1
fi

# 2. Ensure /etc/datachain-agents/shared.env exists.
install -d -m 0755 -o "$USER_NAME" -g "$GROUP_NAME" "$SHARED_ETC"
if [ ! -e "$SHARED_ETC/shared.env" ]; then
    install -m 0644 -o "$USER_NAME" -g "$GROUP_NAME" \
        "$DEPLOY_DIR/env/shared.env.example" "$SHARED_ETC/shared.env"
    echo "  + dropped $SHARED_ETC/shared.env"
fi

# 3. Per-agent /etc/<agent>/ directory + config.env template.
install -d -m 0700 -o "$USER_NAME" -g "$GROUP_NAME" "$ETC_DIR"
if [ ! -e "$ETC_DIR/config.env" ]; then
    install -m 0640 -o "$USER_NAME" -g "$GROUP_NAME" \
        "$DEPLOY_DIR/env/${AGENT}.env.example" "$ETC_DIR/config.env"
    echo "  + dropped $ETC_DIR/config.env"
fi

# 4. Per-agent state dir.
install -d -m 0750 -o "$USER_NAME" -g "$GROUP_NAME" "$STATE_DIR"

# 5. Per-agent signing key (oracle, validation, insurance, compliance).
#    Insurance defers signing to the federation node owning the wallet
#    (see insurance-agent README) but we still pre-create the key file
#    in case a future version uses it.
SEED_FILE=""
case "$AGENT" in
    oracle-agent)     SEED_FILE="$ETC_DIR/oracle.seed" ;;
    validation-agent) SEED_FILE="$ETC_DIR/validation.seed" ;;
    insurance-agent)  SEED_FILE="$ETC_DIR/insurance.seed" ;;
    compliance-agent) SEED_FILE="$ETC_DIR/compliance.seed" ;;
    semantic-agent)   SEED_FILE="" ;;  # SemanticAgent does not sign with its own key
esac

if [ -n "$SEED_FILE" ] && [ ! -e "$SEED_FILE" ]; then
    if "$BIN" init-key --help >/dev/null 2>&1; then
        sudo -u "$USER_NAME" "$BIN" init-key --path "$SEED_FILE"
        chmod 0600 "$SEED_FILE"
        chown "$USER_NAME:$GROUP_NAME" "$SEED_FILE"
        echo "  + generated $SEED_FILE"
    else
        echo "  ! $BIN does not (yet) support 'init-key'; skipping key generation"
        echo "    The agent will fail to start until $SEED_FILE exists."
    fi
fi

# 6. Install the systemd unit.
UNIT_SRC="$DEPLOY_DIR/systemd/${AGENT}.service"
UNIT_DST="/etc/systemd/system/${AGENT}.service"
install -m 0644 -o root -g root "$UNIT_SRC" "$UNIT_DST"
echo "  + installed $UNIT_DST"

# 7. Reload + (re)start.
systemctl daemon-reload
if systemctl is-enabled --quiet "$AGENT"; then
    systemctl restart "$AGENT"
    echo "  + restarted $AGENT"
else
    systemctl enable --now "$AGENT"
    echo "  + enabled + started $AGENT"
fi

# 8. Quick health check.
sleep 2
if systemctl is-active --quiet "$AGENT"; then
    echo "OK  $AGENT is running."
else
    echo "FAIL $AGENT failed to start. Check: journalctl -u $AGENT --since '1 min ago'" >&2
    exit 1
fi

echo
echo "Tail logs with:   sudo journalctl -u $AGENT -f"
echo "View on DCScan:   https://dcscan.io/agents"
