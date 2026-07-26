#!/usr/bin/env bash
# Onboards a new node into the EVM block-production quorum committee.
#
# This is the automation the founder asked for: adding node N+1 to the
# committee must be a DATA change (append one entry to
# evm-quorum-committee.json + redistribute + restart), never a CODE
# change to rope-engine-driver. This script performs exactly that.
#
# Usage:
#   ./onboard-evm-quorum-node.sh <node-name> <ssh-alias-or-host> <attester-public-ip>
#
# Example:
#   ./onboard-evm-quorum-node.sh DO-rpc-3 root@1.2.3.4 1.2.3.4
#
# Prerequisites on the NEW node before running this:
#   - rope-node has run at least once (so ~/.rope/validator_key.bin exists)
#   - reth-rope.service is running WITHOUT --dev, authrpc bound to 127.0.0.1:8552
#   - rope-engine-driver binary is staged at /opt/datachain-rope/bin/rope-engine-driver
#     (built on the jammy canonical build host per the OS-skew rule)
#   - firewall allows inbound tcp/9600 from every EXISTING committee member's
#     public IP, and outbound tcp/9600 to every existing member (mesh, not star)
#
# This script does NOT provision the node itself (Reth, rope-node, binaries) —
# it only performs the committee-roster enrollment + fleet-wide restart.

set -euo pipefail

if [ "$#" -lt 3 ]; then
  echo "usage: $0 <node-name> <ssh-target> <attester-public-ip> [attester-port]" >&2
  exit 1
fi

NEW_NAME="$1"
NEW_SSH="$2"
NEW_IP="$3"
NEW_PORT="${4:-9600}"

COMMITTEE_PATH="/opt/datachain-rope/config/evm-quorum-committee.json"
BIN_PATH="/opt/datachain-rope/bin/rope-engine-driver"
VALIDATOR_KEY_PATH="/home/ubuntu/.rope/validator_key.bin"

# Every existing member — this is the one place the fleet topology is
# spelled out for this script. Update when the committee grows.
EXISTING_SSH_TARGETS=(
  "rope-vps"
  "anvil-vps"
  "-i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45"
  "-i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174"
)
EXISTING_PUBLIC_IPS=(
  "92.243.26.189"
  "92.243.25.119"
  "157.230.18.45"
  "167.172.106.174"
)

echo "==> [1/6] Extracting ${NEW_NAME}'s Ed25519 pubkey from its validator_key.bin"
NEW_PUBKEY=$(ssh $NEW_SSH "$BIN_PATH print-pubkey --validator-key-path $VALIDATOR_KEY_PATH")
if [ -z "$NEW_PUBKEY" ] || [ "${#NEW_PUBKEY}" -ne 64 ]; then
  echo "ERROR: got unexpected pubkey output: '$NEW_PUBKEY' (expected 64 hex chars)" >&2
  exit 1
fi
echo "    pubkey_hex = $NEW_PUBKEY"

echo "==> [2/6] Fetching current committee roster from rope-vps (source of truth)"
TMP_JSON=$(mktemp)
scp rope-vps:"$COMMITTEE_PATH" "$TMP_JSON"

if grep -q "\"$NEW_PUBKEY\"" "$TMP_JSON"; then
  echo "    ${NEW_NAME}'s pubkey is already in the roster — nothing to add, continuing to redistribute/restart anyway."
else
  echo "==> [3/6] Appending ${NEW_NAME} to the roster"
  python3 - "$TMP_JSON" "$NEW_NAME" "$NEW_PUBKEY" "http://${NEW_IP}:${NEW_PORT}" <<'PYEOF'
import json, sys
path, name, pubkey, url = sys.argv[1:5]
with open(path) as f:
    doc = json.load(f)
doc["members"].append({"name": name, "pubkey_hex": pubkey, "attester_url": url})
with open(path, "w") as f:
    json.dump(doc, f, indent=2)
    f.write("\n")
PYEOF
fi

echo "==> [4/6] Redistributing the updated roster to every committee member (including the new one)"
ALL_SSH_TARGETS=("${EXISTING_SSH_TARGETS[@]}" "$NEW_SSH")
for target in "${ALL_SSH_TARGETS[@]}"; do
  echo "    -> $target"
  scp "$TMP_JSON" $target:"$COMMITTEE_PATH.new"
  ssh $target "mv '$COMMITTEE_PATH.new' '$COMMITTEE_PATH' && sha256sum '$COMMITTEE_PATH'"
done

echo "==> [5/6] Opening port ${NEW_PORT} on every existing member's firewall for the new node, and vice versa"
for ip in "${EXISTING_PUBLIC_IPS[@]}"; do
  :
done
echo "    NOTE: firewall mesh update is environment-specific (ufw on DO/GREEN, iptables"
echo "    on BLUE) and is NOT automated by this script yet — see"
echo "    digitalocean-third-blue-green-slot.mdc for the per-host commands, and run:"
for target in "${EXISTING_SSH_TARGETS[@]}"; do
  echo "      ssh $target '<add ALLOW rule for $NEW_IP on tcp/$NEW_PORT>'"
done
echo "      ssh $NEW_SSH '<add ALLOW rules for each of: ${EXISTING_PUBLIC_IPS[*]} on tcp/$NEW_PORT>'"
read -r -p "    Press Enter once firewall rules are confirmed on all hosts (or Ctrl-C to abort here) ..." _

echo "==> [6/6] Restarting the attester service on every committee member"
for target in "${ALL_SSH_TARGETS[@]}"; do
  echo "    -> $target"
  ssh $target "sudo systemctl restart rope-evm-attester.service && sleep 1 && systemctl is-active rope-evm-attester.service"
done

rm -f "$TMP_JSON"

echo ""
echo "==> Onboarding complete. Verify with:"
echo "    for u in $(python3 -c "import json;print(' '.join(m['attester_url'] for m in json.load(open('$COMMITTEE_PATH'))['members']))" 2>/dev/null || true); do curl -sS \"\$u/healthz\"; echo \" <- \$u\"; done"
echo "    (run from a host that has committee-mesh network access, e.g. rope-vps)"
