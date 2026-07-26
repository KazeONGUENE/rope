#!/usr/bin/env bash
# Datachain Rope — Node Deployment Package
#
# Installs and starts a Datachain Rope node on your own VPS/VM, or on a
# Databox device (see https://databox.network). Two profiles:
#
#   full     Local Reth v1.11.2 (synced via rope-engine-driver's Engine-API
#            follower mode) + local rope-node in relay mode. Full copy of
#            chain state. Needs a real VPS/VM (recommend 4 vCPU / 8GB RAM /
#            100GB+ SSD to start, growing over time).
#
#   witness  No local Reth — delegates eth_* RPC to the public, load-
#            balanced erpc.datachain.network endpoint. Runs rope-node only,
#            joins the Testimony gossip mesh. Good fit for Databox-class
#            hardware and small VMs (a few GB disk is enough).
#
# This node NEVER joins the fixed 4-node EVM-quorum committee that
# proposes/attests blocks (BLUE/GREEN/DO-rpc-1/DO-rpc-2) — that committee
# is onboarded separately by the Foundation via
# deploy/scripts/onboard-evm-quorum-node.sh. This package only builds
# read/relay/witness nodes, which is what every third-party operator and
# every Databox device should run.
#
# Usage:
#   sudo ./install.sh --profile witness --name my-first-databox
#   sudo ./install.sh --profile full    --name my-vps-node
#
# See README.md for the full option list and post-install steps
# (registering on the Global Databox Network, checking sync status, etc).

set -euo pipefail

# ---------------------------------------------------------------------------
# Defaults (override with flags)
# ---------------------------------------------------------------------------
PROFILE=""
NODE_NAME="$(hostname -s 2>/dev/null || echo rope-node)"
DATA_DIR="/opt/datachain-rope/data"
BIN_DIR="/opt/datachain-rope/bin"
INSTALL_DIR="/opt/datachain-rope"
SERVICE_USER="rope"
ROPE_REPO="https://github.com/KazeONGUENE/rope.git"
ROPE_REF="main"
RETH_VERSION="v1.11.2"
DEPLOYER_WALLET=""
OPERATOR_NAME=""
OPERATOR_EMAIL=""
OPERATOR_COUNTRY=""
SKIP_FIREWALL="no"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

usage() {
  grep -E '^#( |$)' "${BASH_SOURCE[0]}" | sed -E 's/^# ?//'
  exit 0
}

while [ $# -gt 0 ]; do
  case "$1" in
    --profile) PROFILE="$2"; shift 2 ;;
    --name) NODE_NAME="$2"; shift 2 ;;
    --data-dir) DATA_DIR="$2"; shift 2 ;;
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --install-dir) INSTALL_DIR="$2"; shift 2 ;;
    --user) SERVICE_USER="$2"; shift 2 ;;
    --rope-repo) ROPE_REPO="$2"; shift 2 ;;
    --rope-ref) ROPE_REF="$2"; shift 2 ;;
    --deployer-wallet) DEPLOYER_WALLET="$2"; shift 2 ;;
    --operator-name) OPERATOR_NAME="$2"; shift 2 ;;
    --operator-email) OPERATOR_EMAIL="$2"; shift 2 ;;
    --operator-country) OPERATOR_COUNTRY="$2"; shift 2 ;;
    --skip-firewall) SKIP_FIREWALL="yes"; shift 1 ;;
    -h|--help) usage ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "This installer must run as root (sudo ./install.sh ...)." >&2
  exit 1
fi

if [ "$PROFILE" != "full" ] && [ "$PROFILE" != "witness" ]; then
  echo "ERROR: --profile full|witness is required." >&2
  usage
fi

log() { echo "[node-package] $*"; }

log "Profile: $PROFILE   Node name: $NODE_NAME   Data dir: $DATA_DIR"

# ---------------------------------------------------------------------------
# 1. OS + package deps
# ---------------------------------------------------------------------------
if [ -f /etc/os-release ]; then
  . /etc/os-release
  log "Detected OS: ${PRETTY_NAME:-unknown}"
  case "${VERSION_ID:-}" in
    20.04|22.04) : ;;
    *) log "WARNING: this package is tested on Ubuntu 20.04/22.04. Continuing anyway on ${VERSION_ID:-unknown}." ;;
  esac
fi

ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) RETH_ARCH="x86_64-unknown-linux-gnu" ;;
  aarch64|arm64) RETH_ARCH="aarch64-unknown-linux-gnu" ;;
  *) log "WARNING: unrecognized arch '$ARCH' — will fall back to building Reth from source."; RETH_ARCH="" ;;
esac

log "Installing base packages ..."
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq curl git build-essential pkg-config libssl-dev clang cmake ca-certificates ufw jq python3 python3-pip >/dev/null

# ---------------------------------------------------------------------------
# 2. Service user + directory layout
# ---------------------------------------------------------------------------
if ! id "$SERVICE_USER" >/dev/null 2>&1; then
  log "Creating service user '$SERVICE_USER' ..."
  useradd --system --create-home --shell /usr/sbin/nologin "$SERVICE_USER"
fi

mkdir -p "$DATA_DIR"/{reth/data,reth/logs,rope/db,config}
mkdir -p "$BIN_DIR"
mkdir -p "$INSTALL_DIR/scripts"
chown -R "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR" "$BIN_DIR" "$INSTALL_DIR"

cp "$SCRIPT_DIR/scripts/register-databox.sh" "$SCRIPT_DIR/scripts/heartbeat-databox.sh" "$INSTALL_DIR/scripts/"
chmod +x "$INSTALL_DIR/scripts/register-databox.sh" "$INSTALL_DIR/scripts/heartbeat-databox.sh"
chown -R "$SERVICE_USER:$SERVICE_USER" "$INSTALL_DIR/scripts"

# ---------------------------------------------------------------------------
# 3. Genesis file
# ---------------------------------------------------------------------------
log "Installing genesis.json (chainId 271828) ..."
cp "$SCRIPT_DIR/genesis.json" "$DATA_DIR/reth/genesis.json"
chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR/reth/genesis.json"

# ---------------------------------------------------------------------------
# 4. JWT secret (only needed for the full profile's local Reth)
# ---------------------------------------------------------------------------
if [ "$PROFILE" = "full" ] && [ ! -f "$DATA_DIR/reth/data/jwt.hex" ]; then
  log "Generating Engine API JWT secret ..."
  openssl rand -hex 32 > "$DATA_DIR/reth/data/jwt.hex"
  chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR/reth/data/jwt.hex"
  chmod 600 "$DATA_DIR/reth/data/jwt.hex"
fi

# ---------------------------------------------------------------------------
# 5. Reth v1.11.2 (full profile only) — try prebuilt binary, fall back to
#    building vanilla paradigmxyz/reth from source with the same feature
#    flags production uses (asm_keccak, jemalloc).
# ---------------------------------------------------------------------------
if [ "$PROFILE" = "full" ]; then
  if [ ! -x "$BIN_DIR/reth" ]; then
    RETH_OK="no"
    if [ -n "$RETH_ARCH" ]; then
      RETH_URL="https://github.com/paradigmxyz/reth/releases/download/${RETH_VERSION}/reth-${RETH_VERSION}-${RETH_ARCH}.tar.gz"
      log "Downloading prebuilt Reth ${RETH_VERSION} for ${RETH_ARCH} ..."
      if curl -fsSL "$RETH_URL" -o /tmp/reth.tar.gz; then
        tar -xzf /tmp/reth.tar.gz -C /tmp
        if [ -x /tmp/reth ]; then
          mv /tmp/reth "$BIN_DIR/reth"
          RETH_OK="yes"
        fi
      fi
    fi
    if [ "$RETH_OK" != "yes" ]; then
      log "Prebuilt Reth unavailable — building from source (this takes a while) ..."
      if ! command -v cargo >/dev/null 2>&1; then
        log "Installing Rust toolchain ..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
        # shellcheck disable=SC1090
        source "$HOME/.cargo/env"
      fi
      rm -rf /tmp/reth-src
      git clone --depth 1 --branch "${RETH_VERSION}" https://github.com/paradigmxyz/reth.git /tmp/reth-src
      (cd /tmp/reth-src && cargo build --release --bin reth --features asm_keccak,jemalloc)
      mv /tmp/reth-src/target/release/reth "$BIN_DIR/reth"
    fi
    chmod +x "$BIN_DIR/reth"
    log "Reth installed: $("$BIN_DIR/reth" --version | head -1)"
  else
    log "Reth already present at $BIN_DIR/reth, skipping."
  fi
fi

# ---------------------------------------------------------------------------
# 6. rope-cli + rope-engine-driver — always built from source (this is our
#    own crate, not published as a binary release).
# ---------------------------------------------------------------------------
NEED_ENGINE_DRIVER="no"
[ "$PROFILE" = "full" ] && NEED_ENGINE_DRIVER="yes"

if [ ! -x "$BIN_DIR/rope" ] || { [ "$NEED_ENGINE_DRIVER" = "yes" ] && [ ! -x "$BIN_DIR/rope-engine-driver" ]; }; then
  if ! command -v cargo >/dev/null 2>&1; then
    log "Installing Rust toolchain ..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
    # shellcheck disable=SC1090
    source "$HOME/.cargo/env"
  fi

  if [ -f "$SCRIPT_DIR/../../Cargo.toml" ]; then
    # Running from inside a checked-out datachain-rope monorepo — build in place.
    ROPE_SRC="$(cd "$SCRIPT_DIR/../.." && pwd)"
    log "Building from local monorepo checkout at $ROPE_SRC ..."
  else
    log "Cloning $ROPE_REPO (ref $ROPE_REF) ..."
    rm -rf /tmp/rope-src
    git clone --depth 1 --branch "$ROPE_REF" "$ROPE_REPO" /tmp/rope-src
    ROPE_SRC="/tmp/rope-src"
  fi

  log "Building rope-cli (+ rope-engine-driver for the full profile) — this takes several minutes ..."
  if [ "$NEED_ENGINE_DRIVER" = "yes" ]; then
    (cd "$ROPE_SRC" && cargo build --release -p rope-cli -p rope-engine-driver)
    cp "$ROPE_SRC/target/release/rope-engine-driver" "$BIN_DIR/rope-engine-driver"
  else
    (cd "$ROPE_SRC" && cargo build --release -p rope-cli)
  fi
  cp "$ROPE_SRC/target/release/rope" "$BIN_DIR/rope"
  chmod +x "$BIN_DIR/rope"
  [ -x "$BIN_DIR/rope-engine-driver" ] && chmod +x "$BIN_DIR/rope-engine-driver"
fi

chown -R "$SERVICE_USER:$SERVICE_USER" "$BIN_DIR"

# ---------------------------------------------------------------------------
# 7. Render config
# ---------------------------------------------------------------------------
if [ "$PROFILE" = "full" ]; then
  TEMPLATE="$SCRIPT_DIR/config/rope-full.toml.tmpl"
else
  TEMPLATE="$SCRIPT_DIR/config/rope-witness.toml.tmpl"
fi

log "Rendering node config ..."
sed \
  -e "s|__NODE_NAME__|$NODE_NAME|g" \
  -e "s|__DATA_DIR__|$DATA_DIR|g" \
  -e "s|__DEPLOYER_WALLET__|$DEPLOYER_WALLET|g" \
  -e "s|__OPERATOR_NAME__|$OPERATOR_NAME|g" \
  -e "s|__OPERATOR_EMAIL__|$OPERATOR_EMAIL|g" \
  -e "s|__OPERATOR_COUNTRY__|$OPERATOR_COUNTRY|g" \
  "$TEMPLATE" > "$DATA_DIR/config/rope-node.toml"
chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR/config/rope-node.toml"

# ---------------------------------------------------------------------------
# 8. Render + install systemd units
# ---------------------------------------------------------------------------
log "Installing systemd units ..."

render_unit() {
  local src="$1" dst="$2"
  sed \
    -e "s|__SERVICE_USER__|$SERVICE_USER|g" \
    -e "s|__BIN_DIR__|$BIN_DIR|g" \
    -e "s|__DATA_DIR__|$DATA_DIR|g" \
    -e "s|__INSTALL_DIR__|$INSTALL_DIR|g" \
    "$src" > "$dst"
}

if [ "$PROFILE" = "full" ]; then
  render_unit "$SCRIPT_DIR/systemd/reth-rope.service.tmpl" /etc/systemd/system/reth-rope.service
  render_unit "$SCRIPT_DIR/systemd/rope-evm-follower.service.tmpl" /etc/systemd/system/rope-evm-follower.service
  EVM_DEP="reth-rope.service rope-evm-follower.service"
  ROPE_MODE="relay"
else
  EVM_DEP=""
  ROPE_MODE="validator"
fi

sed \
  -e "s|__SERVICE_USER__|$SERVICE_USER|g" \
  -e "s|__BIN_DIR__|$BIN_DIR|g" \
  -e "s|__DATA_DIR__|$DATA_DIR|g" \
  -e "s|__INSTALL_DIR__|$INSTALL_DIR|g" \
  -e "s|__EVM_DEPENDENCY__|$EVM_DEP|g" \
  -e "s|__ROPE_MODE__|$ROPE_MODE|g" \
  "$SCRIPT_DIR/systemd/datachain-rope-node.service.tmpl" > /etc/systemd/system/datachain-rope-node.service

render_unit "$SCRIPT_DIR/systemd/databox-heartbeat.service.tmpl" /etc/systemd/system/databox-heartbeat.service
cp "$SCRIPT_DIR/systemd/databox-heartbeat.timer" /etc/systemd/system/databox-heartbeat.timer

systemctl daemon-reload

# ---------------------------------------------------------------------------
# 9. Firewall (ufw) — open the P2P port; keep RPC/Engine-API local-only.
# ---------------------------------------------------------------------------
if [ "$SKIP_FIREWALL" != "yes" ] && command -v ufw >/dev/null 2>&1; then
  log "Configuring firewall (opening 9000/tcp for rope-node P2P; RPC ports stay local-only) ..."
  ufw allow 22/tcp >/dev/null 2>&1 || true
  ufw allow 9000/tcp >/dev/null 2>&1 || true
  if ! ufw status | grep -q "Status: active"; then
    log "NOTE: ufw is installed but not active. Run 'ufw enable' yourself when ready — not doing it automatically to avoid locking you out over SSH."
  fi
fi

# ---------------------------------------------------------------------------
# 10. Start services
# ---------------------------------------------------------------------------
if [ "$PROFILE" = "full" ]; then
  log "Starting reth-rope.service ..."
  systemctl enable --now reth-rope.service
  sleep 3
  log "Starting rope-evm-follower.service (this begins mirroring the chain — can take a while for a fresh datadir) ..."
  systemctl enable --now rope-evm-follower.service
fi

log "Starting datachain-rope-node.service ..."
systemctl enable --now datachain-rope-node.service

echo
log "Install complete."
echo
echo "  Profile:       $PROFILE"
echo "  Node config:   $DATA_DIR/config/rope-node.toml"
echo "  Binaries:      $BIN_DIR"
echo
if [ "$PROFILE" = "full" ]; then
  echo "  Check EVM sync progress:"
  echo "    journalctl -u rope-evm-follower.service -f"
  echo "    curl -s -X POST -H 'content-type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}' http://127.0.0.1:8595"
  echo "  Compare against the public head:"
  echo "    curl -s -X POST -H 'content-type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}' https://erpc.datachain.network"
fi
echo "  Check rope-node:"
echo "    journalctl -u datachain-rope-node.service -f"
echo
if [ "$PROFILE" = "full" ]; then
  SUGGESTED_DATABOX_TYPE="rpc_slot"
else
  SUGGESTED_DATABOX_TYPE="witness"
fi
echo "  Register on the Global Databox Network (optional, any profile):"
echo "    $INSTALL_DIR/scripts/register-databox.sh --private-key 0xYOUR_KEY --name \"$NODE_NAME\" --type $SUGGESTED_DATABOX_TYPE --region eu-west"
echo "    sudo systemctl enable --now databox-heartbeat.timer"
echo
echo "  See README.md for troubleshooting and the full option list."
