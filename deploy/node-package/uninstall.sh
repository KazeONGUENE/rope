#!/usr/bin/env bash
# Stops and removes a node installed by install.sh.
# By default keeps $DATA_DIR (chain state, config, your databox.env) —
# pass --purge to delete it too.

set -euo pipefail

DATA_DIR="/opt/datachain-rope/data"
BIN_DIR="/opt/datachain-rope/bin"
INSTALL_DIR="/opt/datachain-rope"
PURGE="no"

while [ $# -gt 0 ]; do
  case "$1" in
    --data-dir) DATA_DIR="$2"; shift 2 ;;
    --bin-dir) BIN_DIR="$2"; shift 2 ;;
    --install-dir) INSTALL_DIR="$2"; shift 2 ;;
    --purge) PURGE="yes"; shift 1 ;;
    *) echo "Unknown argument: $1" >&2; exit 1 ;;
  esac
done

if [ "$(id -u)" -ne 0 ]; then
  echo "Run as root (sudo ./uninstall.sh ...)." >&2
  exit 1
fi

for svc in databox-heartbeat.timer databox-heartbeat.service datachain-rope-node.service rope-evm-follower.service reth-rope.service; do
  systemctl disable --now "$svc" 2>/dev/null || true
done

rm -f /etc/systemd/system/databox-heartbeat.timer \
      /etc/systemd/system/databox-heartbeat.service \
      /etc/systemd/system/datachain-rope-node.service \
      /etc/systemd/system/rope-evm-follower.service \
      /etc/systemd/system/reth-rope.service
systemctl daemon-reload

if [ "$PURGE" = "yes" ]; then
  echo "Purging $DATA_DIR and $BIN_DIR (chain state, config, keys — irreversible) ..."
  rm -rf "$DATA_DIR" "$BIN_DIR"
else
  echo "Services stopped and units removed. Data kept at $DATA_DIR (pass --purge to delete it too)."
fi
