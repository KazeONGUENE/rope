#!/usr/bin/env bash
#
# deploy-shadow-witness.sh
#
# Idempotent build + install + verify of the rope-shadow-witness on one
# target host. Designed to run on rope-vps (BLUE) and SSH to the target.
#
# Usage:
#   deploy-shadow-witness.sh local <witness_tag>
#   deploy-shadow-witness.sh remote <user@host> <witness_tag> [ssh_key_path]
#
# Examples:
#   deploy-shadow-witness.sh local                blue-gandi-paris
#   deploy-shadow-witness.sh remote ubuntu@92.243.25.119 green-gandi-paris ~/.ssh/id_ed25519
#
# Operationally important properties:
#   - Build is performed natively on the target (GLIBC compatibility).
#   - Source is rsync'd from BLUE's `~/datachain-rope/` workspace; only
#     the crates rope-core + rope-shadow-witness + the workspace
#     Cargo.toml are required.
#   - Install layout matches the canary runbook:
#       /usr/local/bin/rope-shadow-witness
#       /etc/rope-shadow-witness/config.toml
#       /var/lib/rope-shadow-witness/data
#       /etc/systemd/system/rope-shadow-witness.service
#   - Each invocation builds with `cargo build --release -p rope-shadow-witness`.
#   - After install, a self-smoke runs: rope_v2_status must respond within
#     20 seconds with `result.spec` containing "Quipu Primitive Canon §6.1.1".
#   - On smoke fail, the service is stopped and the script exits non-zero.

set -euo pipefail

mode="${1:?usage: $0 local|remote ...}"

case "$mode" in
    local)
        TAG="${2:?missing witness_tag}"
        TARGET="(local)"
        SSH_PREFIX=()
        RSYNC_TARGET_PREFIX=""
        IS_REMOTE=false
        ;;
    remote)
        SSH_TARGET="${2:?missing user@host}"
        TAG="${3:?missing witness_tag}"
        SSH_KEY="${4:-}"
        TARGET="$SSH_TARGET"
        if [ -n "$SSH_KEY" ]; then
            SSH_PREFIX=(ssh -i "$SSH_KEY" -o StrictHostKeyChecking=accept-new -o BatchMode=yes "$SSH_TARGET")
            RSYNC_OPTS_E=(-e "ssh -i $SSH_KEY -o StrictHostKeyChecking=accept-new -o BatchMode=yes")
        else
            SSH_PREFIX=(ssh -o BatchMode=yes "$SSH_TARGET")
            RSYNC_OPTS_E=()
        fi
        RSYNC_TARGET_PREFIX="$SSH_TARGET:"
        IS_REMOTE=true
        ;;
    *)
        echo "usage: $0 local|remote ..." >&2
        exit 2
        ;;
esac

run() {
    if $IS_REMOTE; then
        "${SSH_PREFIX[@]}" "$@"
    else
        bash -c "$*"
    fi
}

run_sudo() {
    if $IS_REMOTE; then
        "${SSH_PREFIX[@]}" "sudo $*"
    else
        sudo bash -c "$*"
    fi
}

remote_run_script() {
    if $IS_REMOTE; then
        "${SSH_PREFIX[@]}" 'bash -s' < /dev/stdin
    else
        bash -s
    fi
}

echo "============================================================"
echo "rope-shadow-witness deploy"
echo "  target:       $TARGET"
echo "  witness_tag:  $TAG"
echo "  ts:           $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "============================================================"

# ---------------------------------------------------------------------------
# 1. Prepare a minimal source tree on BLUE under /tmp/shadow-source-pkg/
# ---------------------------------------------------------------------------
SRC_ROOT="${HOME}/datachain-rope"
PKG_DIR="/tmp/shadow-source-pkg"

if [ ! -d "$SRC_ROOT/crates/rope-shadow-witness" ]; then
    echo "FATAL: $SRC_ROOT/crates/rope-shadow-witness missing on BLUE." >&2
    exit 1
fi

rm -rf "$PKG_DIR"
mkdir -p "$PKG_DIR/crates"
cp -a "$SRC_ROOT/crates/rope-core"           "$PKG_DIR/crates/"
cp -a "$SRC_ROOT/crates/rope-shadow-witness" "$PKG_DIR/crates/"
cat > "$PKG_DIR/Cargo.toml" <<'TOML'
[workspace]
resolver = "2"
members = ["crates/rope-core", "crates/rope-shadow-witness"]

[workspace.package]
version = "0.1.0"
edition = "2021"
authors = ["Datachain Foundation"]
license = "Apache-2.0"

[workspace.dependencies]
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
config = "0.14"
clap = { version = "4", features = ["derive"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
thiserror = "1"
anyhow = "1"
hex = "0.4"
chrono = { version = "0.4", features = ["serde"] }
parking_lot = "0.12"
blake3 = "1"
rocksdb = "0.22"
reqwest = { version = "0.12", features = ["json", "rustls-tls"], default-features = false }
prometheus = "0.13"
hashbrown = "0.14"
tempfile = "3"
rayon = "1.10"
TOML

# ---------------------------------------------------------------------------
# 2. Stage source on the target
# ---------------------------------------------------------------------------
echo "[1/5] staging source on $TARGET..."
if $IS_REMOTE; then
    "${SSH_PREFIX[@]}" 'sudo mkdir -p /opt/shadow-build && sudo chown $(id -u):$(id -g) /opt/shadow-build'
    rsync -az --delete "${RSYNC_OPTS_E[@]}" "$PKG_DIR/" "${RSYNC_TARGET_PREFIX}/opt/shadow-build/"
else
    sudo mkdir -p /opt/shadow-build
    sudo chown "$(id -u):$(id -g)" /opt/shadow-build
    rsync -az --delete "$PKG_DIR/" /opt/shadow-build/
fi

# ---------------------------------------------------------------------------
# 3. Build
# ---------------------------------------------------------------------------
echo "[2/5] building rope-shadow-witness on $TARGET (this may take 10-30 minutes on first build)..."
remote_run_script <<REMOTE
set -euo pipefail
cd /opt/shadow-build
export CARGO_HOME="\${CARGO_HOME:-\$HOME/.cargo}"
export PATH="\$CARGO_HOME/bin:\$PATH"
if ! command -v cargo >/dev/null; then
    echo "FATAL: cargo not on PATH on target" >&2; exit 1
fi
cargo build --release -p rope-shadow-witness 2>&1 | tail -3
[ -x ./target/release/rope-shadow-witness ] || { echo "FATAL: binary missing"; exit 1; }
ls -la ./target/release/rope-shadow-witness
REMOTE

# ---------------------------------------------------------------------------
# 4. Install
# ---------------------------------------------------------------------------
echo "[3/5] installing on $TARGET..."
remote_run_script <<REMOTE
set -euo pipefail
sudo install -o root -g root -m 0755 /opt/shadow-build/target/release/rope-shadow-witness /usr/local/bin/rope-shadow-witness
sudo mkdir -p /etc/rope-shadow-witness /var/lib/rope-shadow-witness/data /var/log
sudo chown -R root:root /var/lib/rope-shadow-witness
sudo cat > /tmp/rope-shadow-witness.config.toml <<CFG
upstream_rpc_url = "https://erpc.datachain.network"
data_dir         = "/var/lib/rope-shadow-witness/data"
bind_addr        = "127.0.0.1:8556"
poll_interval_secs = 10
strings_per_round  = 200
witness_tag        = "${TAG}"
CFG
sudo install -o root -g root -m 0644 /tmp/rope-shadow-witness.config.toml /etc/rope-shadow-witness/config.toml
rm -f /tmp/rope-shadow-witness.config.toml

sudo cat > /tmp/rope-shadow-witness.service <<'UNIT'
[Unit]
Description=rope-shadow-witness — Quipu Canon §6.1.1 v2 shadow chain witness
Documentation=https://github.com/Datachain-Foundation/datachain-rope
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=/usr/local/bin/rope-shadow-witness --config /etc/rope-shadow-witness/config.toml
Restart=on-failure
RestartSec=5s
User=root
Group=root
StandardOutput=journal
StandardError=journal
LimitNOFILE=1048576

# Hardening (does not impede operation; restricts blast radius)
NoNewPrivileges=yes
ProtectSystem=full
ProtectHome=read-only
PrivateTmp=yes
ReadWritePaths=/var/lib/rope-shadow-witness

[Install]
WantedBy=multi-user.target
UNIT
sudo install -o root -g root -m 0644 /tmp/rope-shadow-witness.service /etc/systemd/system/rope-shadow-witness.service
rm -f /tmp/rope-shadow-witness.service

sudo systemctl daemon-reload
sudo systemctl enable rope-shadow-witness >/dev/null 2>&1 || true
sudo systemctl restart rope-shadow-witness
sleep 3
sudo systemctl is-active rope-shadow-witness
REMOTE

# ---------------------------------------------------------------------------
# 5. Self-smoke: rope_v2_status round-trip on local 127.0.0.1:8556
# ---------------------------------------------------------------------------
echo "[4/5] running self-smoke on $TARGET..."
SMOKE_OK=false
for attempt in 1 2 3 4 5; do
    if remote_run_script <<'REMOTE_SMOKE' >/dev/null 2>&1
set -euo pipefail
RES=$(curl -sS --max-time 5 -X POST -H 'Content-Type: application/json' \
    -d '{"jsonrpc":"2.0","method":"rope_v2_status","params":[],"id":1}' \
    http://127.0.0.1:8556)
echo "$RES" | grep -q "Quipu Primitive Canon"
REMOTE_SMOKE
    then
        SMOKE_OK=true; break
    fi
    sleep 4
done

if ! $SMOKE_OK; then
    echo "[5/5] SMOKE_FAIL on $TARGET — rolling back service."
    remote_run_script <<'REMOTE'
sudo systemctl stop rope-shadow-witness || true
REMOTE
    exit 3
fi

echo "[5/5] SMOKE_PASS on $TARGET — service active and responding."
echo "============================================================"
echo "DEPLOY_OK $TARGET ($TAG)"
echo "============================================================"
