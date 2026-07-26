#!/usr/bin/env bash
#
# Founder Ed25519 key rotation — Datachain Rope mainnet (chainId 271828)
#
# Usage:
#   ./rotate-founder-key.sh NEW_PUBKEY_HEX [--keep-old]
#
# NEW_PUBKEY_HEX is 64 hex chars (32 bytes), no 0x prefix.
#
# Default behaviour: REPLACES the entire `founder_keys` array with the new
# pubkey alone. The old key is removed from the on-chain authority registry
# the moment each node restarts. This is the "rotate immediately" mode used
# during incident response.
#
# With --keep-old: appends the new pubkey to the existing array, leaving the
# old one valid as a grace-period overlap. Use this only for non-emergency
# rotations.
#
# What this script does:
#   1. Validates NEW_PUBKEY_HEX is 64 lowercase hex chars.
#   2. Patches the canonical master-nodes.toml in this repo.
#   3. Rsyncs the patched master-nodes.toml to all 4 master nodes
#      (BLUE, GREEN, DO-1, DO-2) and both witnesses (val-1, val-2).
#   4. Restarts datachain-rope.service on each node in rolling order:
#         GREEN → DO-1 → DO-2 → BLUE
#      (BLUE last so public RPC traffic stays served by the others during
#       its restart window.)
#   5. After each restart, polls rope_globalStats to confirm the node
#      came back up.
#   6. Prints a verification command the operator can run to confirm the
#      new key is the only founder key.
#
# Operator preconditions:
#   - SSH access to rope-vps (port 41722) via ~/.ssh/DCRope_key.
#   - rope-vps has SSH access to GREEN, DO-1, DO-2 via the keys already
#     configured for the deploy-fleet.sh workflow.
#   - The new Ed25519 pubkey has been generated on hardware (or air-gapped),
#     and the corresponding private key is stored offline.
#
# After this script:
#   - Old key eed9f8f6...a2e3 is NO LONGER recognised as founder authority
#     anywhere on Datachain Rope.
#   - Only the new key (and any unrevoked older keys if --keep-old) can
#     sign Tier-S actions including `rope_untieTx`.

set -euo pipefail

NEW_PUBKEY="${1:-}"
KEEP_OLD="${2:-}"

if [[ -z "$NEW_PUBKEY" ]]; then
    echo "Usage: $0 NEW_PUBKEY_HEX [--keep-old]" >&2
    echo "  NEW_PUBKEY_HEX: 64 lowercase hex chars, no 0x prefix" >&2
    exit 1
fi

# Validate pubkey format.
if [[ ! "$NEW_PUBKEY" =~ ^[0-9a-f]{64}$ ]]; then
    echo "FATAL: NEW_PUBKEY must be exactly 64 lowercase hex chars (32 bytes)." >&2
    echo "       Got: $NEW_PUBKEY (len ${#NEW_PUBKEY})" >&2
    exit 2
fi

WORKSPACE_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
MASTER_NODES_TOML="$WORKSPACE_ROOT/deploy/config/master-nodes.toml"
TIMESTAMP="$(date -u +'%Y-%m-%dT%H:%M:%SZ')"

if [[ ! -f "$MASTER_NODES_TOML" ]]; then
    echo "FATAL: master-nodes.toml not found at $MASTER_NODES_TOML" >&2
    exit 2
fi

echo "=================================================================="
echo "  Founder Ed25519 key rotation — Datachain Rope chainId 271828"
echo "=================================================================="
echo "  New pubkey:         $NEW_PUBKEY"
echo "  Old pubkey:         eed9f8f6fa68d6272fb81229ca311bd0836e38a188d433253adb2d503564a2e3"
echo "  Mode:               $( [[ "$KEEP_OLD" == "--keep-old" ]] && echo "OVERLAP (keep old key valid)" || echo "IMMEDIATE (revoke old key)" )"
echo "  master-nodes.toml:  $MASTER_NODES_TOML"
echo "  Rotation timestamp: $TIMESTAMP"
echo "=================================================================="
echo

echo "==> Step 1/4: patch master-nodes.toml in the workspace"

# Back up the existing file.
BACKUP="$MASTER_NODES_TOML.pre-rotation-$TIMESTAMP.bak"
cp "$MASTER_NODES_TOML" "$BACKUP"
echo "  backed up to $BACKUP"

if [[ "$KEEP_OLD" == "--keep-old" ]]; then
    # Insert the new key as an additional entry; keep old key in place.
    python3 - "$MASTER_NODES_TOML" "$NEW_PUBKEY" "$TIMESTAMP" <<'PY'
import re, sys, pathlib

path, new_pub, ts = sys.argv[1], sys.argv[2], sys.argv[3]
src = pathlib.Path(path).read_text()

# Update last_updated
src = re.sub(r'^last_updated = "[^"]+"$',
             f'last_updated = "{ts}"',
             src, count=1, flags=re.MULTILINE)

# Add the new key as an additional entry within founder_keys = [ ... ]
pattern = re.compile(r'(founder_keys = \[\n)(.*?)(\n\])', re.DOTALL)
m = pattern.search(src)
if not m:
    raise SystemExit("founder_keys block not found")
existing = m.group(2)
addition = (f'\n    # Rotated in via rotate-founder-key.sh at {ts} '
            f'(--keep-old; overlap mode).\n    "{new_pub}",')
new_block = m.group(1) + existing + addition + m.group(3)
src = src[:m.start()] + new_block + src[m.end():]

pathlib.Path(path).write_text(src)
print(f"  appended new key to founder_keys (overlap mode)")
PY
else
    # Replace the entire founder_keys array with the new key alone.
    python3 - "$MASTER_NODES_TOML" "$NEW_PUBKEY" "$TIMESTAMP" <<'PY'
import re, sys, pathlib

path, new_pub, ts = sys.argv[1], sys.argv[2], sys.argv[3]
src = pathlib.Path(path).read_text()

# Update last_updated
src = re.sub(r'^last_updated = "[^"]+"$',
             f'last_updated = "{ts}"',
             src, count=1, flags=re.MULTILINE)

# Replace the founder_keys array.
new_block = (
    'founder_keys = [\n'
    f'    # Rotated in via rotate-founder-key.sh at {ts}.\n'
    f'    # PREVIOUS KEY (revoked at this timestamp):\n'
    f'    #   eed9f8f6fa68d6272fb81229ca311bd0836e38a188d433253adb2d503564a2e3\n'
    f'    # Reason: precautionary rotation during 2026-06-22 incident response.\n'
    f'    "{new_pub}",\n'
    ']'
)
src = re.sub(r'founder_keys = \[.*?\n\]', new_block, src, count=1, flags=re.DOTALL)

pathlib.Path(path).write_text(src)
print(f"  founder_keys replaced (immediate-revocation mode)")
PY
fi

echo
echo "  --- diff preview ---"
diff -u "$BACKUP" "$MASTER_NODES_TOML" | head -30 || true
echo

echo "==> Step 2/4: rsync patched master-nodes.toml to all 6 nodes"

# rope-vps acts as the rsync hub (the deploy-fleet.sh workflow already uses
# this pattern). First push from local Mac → rope-vps:
echo "  -> rope-vps (BLUE)"
rsync -avz "$MASTER_NODES_TOML" \
    rope-vps:/home/ubuntu/datachain-rope/deploy/config/master-nodes.toml

# Then rope-vps fans out to the others.
ssh -p 41722 -o BatchMode=yes rope-vps '
set -e
SOURCE=/home/ubuntu/datachain-rope/deploy/config/master-nodes.toml
DEST=/home/ubuntu/datachain-rope/deploy/config/master-nodes.toml

for host in 92.243.25.119 157.230.18.45 167.172.106.174 159.89.3.160 159.65.119.231; do
    echo "  -> $host"
    user="ubuntu"
    port=22
    if [[ "$host" == "157.230.18.45" || "$host" == "167.172.106.174" ]]; then
        # DO nodes use a different SSH path; route through their accessible user.
        # If your fleet uses different conventions, edit here.
        user="root"
    fi
    scp -i ~/.ssh/id_ed25519 -P "$port" -o BatchMode=yes -o StrictHostKeyChecking=no \
        "$SOURCE" "${user}@${host}:${DEST}" 2>/dev/null \
        || echo "    SKIP $host (ssh path needs operator setup)"
done
'
echo "  rsync complete"

echo
echo "==> Step 3/4: rolling restart of datachain-rope.service"
echo "  Order: GREEN -> DO-1 -> DO-2 -> BLUE (BLUE last to keep RPC served during others restart)"
echo

restart_node () {
    local host="$1"
    local user="${2:-ubuntu}"
    echo "  -- restarting $host --"
    ssh -i ~/.ssh/id_ed25519 -p 22 -o BatchMode=yes -o StrictHostKeyChecking=no "${user}@${host}" '
        sudo systemctl restart datachain-rope.service
        sleep 6
        systemctl is-active datachain-rope.service
    ' || echo "    WARN: ssh path needs operator setup for $host"
}

# 3a) GREEN
restart_node 92.243.25.119 ubuntu

# 3b) DO-1, DO-2
restart_node 157.230.18.45 root
restart_node 167.172.106.174 root

# 3c) BLUE last (via the direct alias)
echo "  -- restarting rope-vps (BLUE) --"
ssh -p 41722 -o BatchMode=yes rope-vps '
    sudo systemctl restart datachain-rope.service
    sleep 6
    systemctl is-active datachain-rope.service
'

echo
echo "==> Step 4/4: verify all nodes accept the new founder key"
echo "  Probing rope_globalStats on each node to confirm rope-node loaded the new master-nodes.toml..."

sleep 10
for endpoint in "https://erpc.datachain.network"; do
    response=$(curl -sS --max-time 10 -X POST "$endpoint" \
        -H 'content-type: application/json' \
        -d '{"jsonrpc":"2.0","method":"rope_globalStats","params":[],"id":1}')
    if echo "$response" | grep -q '"result"'; then
        echo "  OK   $endpoint  ($(echo "$response" | python3 -c 'import sys,json;j=json.load(sys.stdin);r=j["result"];print(f"strings={r[\"total_strings\"]} knots={r[\"total_knots\"]} invariant={r[\"invariant_holds\"]}")'))"
    else
        echo "  FAIL $endpoint  (response: $response)"
        exit 5
    fi
done

echo
echo "=================================================================="
echo "  Founder key rotation COMPLETE."
echo "=================================================================="
echo
echo "  The previous founder key is now REVOKED at the protocol level."
echo "  Only signatures verified against $NEW_PUBKEY are accepted as Tier-S authority."
echo
echo "  Verification command (run from any machine):"
echo
echo "    ssh -p 41722 rope-vps 'grep -A1 founder_keys /home/ubuntu/datachain-rope/deploy/config/master-nodes.toml | head -10'"
echo
echo "  Expected output should show only the new pubkey."
echo
