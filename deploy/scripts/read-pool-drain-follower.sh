#!/usr/bin/env bash
# read-pool-drain-follower.sh - Temporarily remove a follower from London nginx read pools.
#
# Used by reth-snapshot-replicate.sh before stopping reth/datachain-rope on a follower
# so public RPC does not route reads to a host that is mid-rsync (~300s outage).
#
# Runs on the writer/edge host (London new-blue). Requires docker rope-nginx.
#
# Usage:
#   read-pool-drain-follower.sh drain   GREEN|DOrpc1|DOrpc2|ParisLegacy
#   read-pool-drain-follower.sh undrain GREEN|DOrpc1|DOrpc2|ParisLegacy
#   read-pool-drain-follower.sh status  GREEN|DOrpc1|DOrpc2|ParisLegacy
#
# Include files live under:
#   $NGINX_CONF_ROOT/includes/read-pool/{green,do1,do2,paris}.{rpc,ws,attesters}.inc
# Active templates ship in deploy/nginx/conf.d/includes/read-pool/*.inc (repo).

set -euo pipefail

ACTION="${1:?usage: drain|undrain|status <follower>}"
FOLLOWER="${2:?usage: GREEN|DOrpc1|DOrpc2|ParisLegacy}"

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
NGINX_CONF_ROOT="${ROPE_NGINX_CONF_ROOT:-/opt/datachain-rope/code/deploy/nginx/conf.d}"
INCLUDE_DIR="$NGINX_CONF_ROOT/includes/read-pool"
TEMPLATE_DIR="${ROPE_READ_POOL_TEMPLATE_DIR:-$SCRIPT_DIR/../nginx/conf.d/includes/read-pool}"
NGINX_CONTAINER="${ROPE_NGINX_CONTAINER:-rope-nginx}"

declare -A FOLLOWER_PREFIX=(
  [GREEN]=green
  [DOrpc1]=do1
  [DOrpc2]=do2
  [ParisLegacy]=paris
)

if [[ -z "${FOLLOWER_PREFIX[$FOLLOWER]+x}" ]]; then
  echo "unknown follower: $FOLLOWER" >&2
  exit 2
fi

prefix="${FOLLOWER_PREFIX[$FOLLOWER]}"
mkdir -p "$INCLUDE_DIR"

reload_nginx() {
  if ! docker ps --format '{{.Names}}' | grep -qx "$NGINX_CONTAINER"; then
    echo "WARN: nginx container $NGINX_CONTAINER not running; skip reload" >&2
    return 0
  fi
  docker exec "$NGINX_CONTAINER" nginx -t
  docker exec "$NGINX_CONTAINER" nginx -s reload
}

pool_file() {
  echo "$INCLUDE_DIR/${prefix}.$1.inc"
}

is_drained() {
  local f
  f="$(pool_file "$1")"
  [[ -f "$f" ]] && grep -q 'ROPE_READ_POOL_DRAINED' "$f" 2>/dev/null
}

drain_pool() {
  local pool="$1"
  local f
  f="$(pool_file "$pool")"
  printf '# ROPE_READ_POOL_DRAINED %s follower=%s pool=%s\n' \
    "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$FOLLOWER" "$pool" >"$f"
}

undrain_pool() {
  local pool="$1"
  local src dst
  src="$TEMPLATE_DIR/${prefix}.${pool}.inc"
  dst="$(pool_file "$pool")"
  if [[ ! -f "$src" ]]; then
    echo "missing template: $src" >&2
    exit 1
  fi
  cp "$src" "$dst"
}

case "$ACTION" in
  drain)
    for pool in rpc ws attesters; do
      drain_pool "$pool"
    done
    reload_nginx
    echo "drained $FOLLOWER from read pools"
    ;;
  undrain)
    for pool in rpc ws attesters; do
      undrain_pool "$pool"
    done
    reload_nginx
    echo "restored $FOLLOWER in read pools"
    ;;
  status)
    for pool in rpc ws attesters; do
      if is_drained "$pool"; then
        echo "$FOLLOWER.$pool: drained"
      else
        echo "$FOLLOWER.$pool: active"
      fi
    done
    ;;
  *)
    echo "unknown action: $ACTION" >&2
    exit 2
    ;;
esac
