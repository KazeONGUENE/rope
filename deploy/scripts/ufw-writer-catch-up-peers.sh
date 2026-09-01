#!/usr/bin/env bash
# ufw-writer-catch-up-peers.sh - Allow attester followers to reach London Reth :8595.
#
# Run once on London (new-blue) after writer migration. Idempotent (ufw allow is safe to repeat).
#
# Peers: GREEN, Paris legacy, DO-rpc-1, DO-rpc-2.

set -euo pipefail

PEERS=(
  92.243.25.119   # GREEN
  92.243.26.189   # Paris legacy
  157.230.18.45   # DO-rpc-1
  167.172.106.174 # DO-rpc-2
)

for ip in "${PEERS[@]}"; do
  ufw allow from "$ip" to any port 8595 proto tcp comment "rope attester catch-up" || true
done

ufw status numbered | grep 8595 || true
echo "done"
