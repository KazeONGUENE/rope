#!/usr/bin/env bash
# provision-testnet-droplet.sh - Datachain Rope testnet dedicated host.
#
# Provisions rope-testnet-1 on DigitalOcean (lon1, s-2vcpu-4gb,
# Ubuntu 24.04) with a firewall that exposes 22 + 9000 (testnet libp2p,
# natural port on a dedicated host) + 443 (nginx TLS) publicly and
# keeps 8545/8546/8547/8595 private.
#
# Idempotent: re-running finds the existing droplet / firewall by name
# and prints their IDs without recreating them. Safe to run repeatedly.
#
# Requires DIGITALOCEAN_TOKEN in the environment. Makes ZERO API calls
# without it and prints exactly what it would do (with --dry-run).
#
# Usage:
#   export DIGITALOCEAN_TOKEN=dop_v1_...
#   ./provision-testnet-droplet.sh                 # provision
#   ./provision-testnet-droplet.sh --dry-run       # plan only
#   ./provision-testnet-droplet.sh --name rope-testnet-1 --region lon1
#
# Rationale for the dedicated host is documented in
# `.cursor/rules/handover-dedicated-testnet-host-2026-08-31.mdc`
# and the design doc
# `datachain-rope/docs/design/rope-testnet-writer-facade.md`.

set -euo pipefail

NAME="rope-testnet-1"
REGION="lon1"
SIZE="s-2vcpu-4gb"
IMAGE="ubuntu-24-04-x64"   # matches new-blue + rope-vps glibc 2.39
SSH_KEY_NAME="datachain-rope-key"
FW_NAME="rope-testnet-fw"
TAG="rope-testnet"
DRY_RUN=0
OUT_MANIFEST="rope-testnet-1.provision.json"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --name) NAME="$2"; shift 2;;
    --region) REGION="$2"; shift 2;;
    --size) SIZE="$2"; shift 2;;
    --image) IMAGE="$2"; shift 2;;
    --ssh-key) SSH_KEY_NAME="$2"; shift 2;;
    --out) OUT_MANIFEST="$2"; shift 2;;
    --dry-run) DRY_RUN=1; shift;;
    *) echo "unknown arg: $1"; exit 2;;
  esac
done

if [[ -z "${DIGITALOCEAN_TOKEN:-}" ]]; then
  echo "ERROR: DIGITALOCEAN_TOKEN is not set."
  echo "  export DIGITALOCEAN_TOKEN=dop_v1_...   then re-run."
  echo "  (This script performs zero API calls without it.)"
  exit 1
fi

API="https://api.digitalocean.com/v2"
AUTH="Authorization: Bearer $DIGITALOCEAN_TOKEN"

say()  { echo "[$(date +%H:%M:%S)] $*"; }
plan() { echo "  PLAN: $*"; }

# --- python json helper ---
py_extract() {
  # $1 = python expr against a var `d` bound to stdin JSON
  python3 -c "import json,sys
try:
    d=json.load(sys.stdin)
except Exception:
    d={}
try:
    print($1)
except Exception:
    pass"
}

say "Testnet host provisioner - name=$NAME region=$REGION size=$SIZE image=$IMAGE dry_run=$DRY_RUN"

# --- 0. Sanity: SSH key exists on DO account -----------------------
SSH_KEY_ID=$(curl -s -X GET "$API/account/keys" -H "$AUTH" | py_extract "
next((k['id'] for k in d.get('ssh_keys',[]) if k['name']=='$SSH_KEY_NAME'), '')
")
if [[ -z "$SSH_KEY_ID" ]]; then
  echo "ERROR: SSH key '$SSH_KEY_NAME' not found on the DO account."
  echo "  Existing keys on the account:"
  curl -s -X GET "$API/account/keys" -H "$AUTH" \
    | py_extract "'\n'.join('    - '+k['name']+' (id='+str(k['id'])+')' for k in d.get('ssh_keys',[]))"
  echo "  Add 'datachain-rope-key' in the DO console or via API before proceeding."
  exit 1
fi
say "SSH key id: $SSH_KEY_ID  ($SSH_KEY_NAME)"

# --- 1. Firewall (single-box, no VPC) ------------------------------
# Exposed to internet:
#   22/tcp   - SSH
#   443/tcp  - HTTPS (nginx terminating TLS locally on rope-testnet-1)
#   9000/tcp - libp2p tcp (natural port; testnet is on a dedicated
#              host so there is no conflict with mainnet's :9000 on
#              new-blue, which listens on a different public IP)
#   9000/udp - libp2p udp (same rationale)
# Firewalled (loopback-only, enforced by rope-node + Reth binds):
#   8545/tcp - rope-testnet-node HTTP (facade)
#   8546/tcp - rope-testnet-node WS
#   8547/tcp - Reth WS
#   8595/tcp - Reth HTTP
# We still deny these explicitly at the DO firewall layer so a mis-
# configured bind cannot leak.

FW_ID=$(curl -s -X GET "$API/firewalls" -H "$AUTH" | py_extract "
next((f['id'] for f in d.get('firewalls',[]) if f['name']=='$FW_NAME'), '')
")
if [[ -z "$FW_ID" ]]; then
  if [[ $DRY_RUN -eq 1 ]]; then
    plan "create firewall $FW_NAME (22, 443, 9000 tcp+udp public; RPC ports private-by-bind)"
  else
    # Capture the FULL response so we can surface the id AND fail loudly
    # if the API returned an error object (a prior version of this script
    # silently swallowed the response and continued as if the firewall
    # existed, which left the droplet exposed at the cloud layer for the
    # window between droplet-create and the operator noticing).
    FW_RESP=$(curl -s -X POST "$API/firewalls" -H "$AUTH" -H "Content-Type: application/json" -d "{
      \"name\":\"$FW_NAME\",
      \"inbound_rules\":[
        {\"protocol\":\"tcp\",\"ports\":\"22\",\"sources\":{\"addresses\":[\"0.0.0.0/0\",\"::/0\"]}},
        {\"protocol\":\"tcp\",\"ports\":\"443\",\"sources\":{\"addresses\":[\"0.0.0.0/0\",\"::/0\"]}},
        {\"protocol\":\"tcp\",\"ports\":\"9000\",\"sources\":{\"addresses\":[\"0.0.0.0/0\",\"::/0\"]}},
        {\"protocol\":\"udp\",\"ports\":\"9000\",\"sources\":{\"addresses\":[\"0.0.0.0/0\",\"::/0\"]}}
      ],
      \"outbound_rules\":[
        {\"protocol\":\"tcp\",\"ports\":\"all\",\"destinations\":{\"addresses\":[\"0.0.0.0/0\",\"::/0\"]}},
        {\"protocol\":\"udp\",\"ports\":\"all\",\"destinations\":{\"addresses\":[\"0.0.0.0/0\",\"::/0\"]}},
        {\"protocol\":\"icmp\",\"destinations\":{\"addresses\":[\"0.0.0.0/0\",\"::/0\"]}}
      ],
      \"tags\":[\"$TAG\"]
    }")
    FW_ID=$(printf '%s' "$FW_RESP" | py_extract "d.get('firewall',{}).get('id','')")
    if [[ -z "$FW_ID" ]]; then
      echo "ERROR: firewall creation failed. API response:"
      printf '%s\n' "$FW_RESP"
      echo "  Refusing to continue - a droplet without a cloud-layer firewall"
      echo "  would leak any 0.0.0.0-bound listener the moment services start."
      exit 1
    fi
    say "created firewall $FW_ID"
  fi
else
  say "firewall exists: $FW_ID"
fi

# --- 2. Droplet ----------------------------------------------------
DROPLET_LIST=$(curl -s -X GET "$API/droplets?tag_name=$TAG&per_page=200" -H "$AUTH")
DROPLET_ID=$(echo "$DROPLET_LIST" | py_extract "
next((dr['id'] for dr in d.get('droplets',[]) if dr['name']=='$NAME'), '')
")

if [[ -n "$DROPLET_ID" ]]; then
  say "droplet $NAME exists (id=$DROPLET_ID) - skipping create"
else
  if [[ $DRY_RUN -eq 1 ]]; then
    plan "create droplet $NAME ($SIZE, $IMAGE) in $REGION with SSH key $SSH_KEY_NAME"
  else
    DROPLET_ID=$(curl -s -X POST "$API/droplets" -H "$AUTH" -H "Content-Type: application/json" -d "{
      \"name\":\"$NAME\",
      \"region\":\"$REGION\",
      \"size\":\"$SIZE\",
      \"image\":\"$IMAGE\",
      \"ssh_keys\":[$SSH_KEY_ID],
      \"tags\":[\"$TAG\"],
      \"monitoring\":true,
      \"backups\":false,
      \"ipv6\":true
    }" | py_extract "d.get('droplet',{}).get('id','')")
    if [[ -z "$DROPLET_ID" ]]; then
      echo "ERROR: droplet creation failed. API response:"
      curl -s -X POST "$API/droplets" -H "$AUTH" -H "Content-Type: application/json" -d "{
        \"name\":\"$NAME\",\"region\":\"$REGION\",\"size\":\"$SIZE\",\"image\":\"$IMAGE\",
        \"ssh_keys\":[$SSH_KEY_ID],\"tags\":[\"$TAG\"]
      }"
      exit 1
    fi
    say "created droplet $NAME (id=$DROPLET_ID)"
  fi
fi

if [[ $DRY_RUN -eq 1 ]]; then
  say "dry-run complete - no resources changed."
  exit 0
fi

# --- 3. Attach firewall to droplet (idempotent + verified) ---------
# Historical bug: the previous version fired-and-forgot with `|| true`
# and never confirmed the attachment. On the initial provisioning run
# (2026-08-31) the firewall never got created *and* the attach step
# silently succeeded against a stale/empty $FW_ID, so the droplet was
# briefly exposed until manually reconciled. Now: post to the attach
# endpoint, then GET the firewall and assert droplet_ids includes us.
say "attaching firewall $FW_ID to droplet $DROPLET_ID (idempotent)"
ATTACH_RESP=$(curl -s -X POST "$API/firewalls/$FW_ID/droplets" -H "$AUTH" -H "Content-Type: application/json" \
  -d "{\"droplet_ids\":[$DROPLET_ID]}")
# A successful attach returns 204 No Content (empty body). An error
# returns a JSON error object. Empty body = success.
if [[ -n "$ATTACH_RESP" ]]; then
  # We got something back - inspect it. If it's an error, bail.
  ATTACH_ERR=$(printf '%s' "$ATTACH_RESP" | py_extract "d.get('message','') or d.get('id','')")
  if [[ -n "$ATTACH_ERR" ]]; then
    echo "ERROR: firewall attach returned an error object: $ATTACH_ERR"
    printf '%s\n' "$ATTACH_RESP"
    exit 1
  fi
fi
# Verify by reading the firewall back
sleep 2
ATTACHED=$(curl -s -X GET "$API/firewalls/$FW_ID" -H "$AUTH" \
  | py_extract "'yes' if $DROPLET_ID in d.get('firewall',{}).get('droplet_ids',[]) else 'no'")
if [[ "$ATTACHED" != "yes" ]]; then
  echo "ERROR: firewall $FW_ID is NOT attached to droplet $DROPLET_ID."
  echo "  Refusing to continue - the droplet must never be reachable"
  echo "  without the DO firewall in front of it."
  exit 1
fi
say "firewall attach verified: droplet $DROPLET_ID is behind $FW_NAME"

# --- 4. Wait for public IP + emit manifest -------------------------
say "waiting up to 90s for public IP assignment..."
for i in $(seq 1 18); do
  INFO=$(curl -s -X GET "$API/droplets/$DROPLET_ID" -H "$AUTH")
  PUB_IP=$(echo "$INFO" | py_extract "
next((n['ip_address'] for n in d.get('droplet',{}).get('networks',{}).get('v4',[]) if n['type']=='public'), '')
")
  if [[ -n "$PUB_IP" ]]; then
    say "public IPv4 assigned: $PUB_IP"
    break
  fi
  sleep 5
done

if [[ -z "$PUB_IP" ]]; then
  echo "WARN: no public IP after 90s - check DO console for droplet $DROPLET_ID."
  PUB_IP="pending"
fi

curl -s -X GET "$API/droplets/$DROPLET_ID" -H "$AUTH" \
  | python3 -c "
import json,sys
d=json.load(sys.stdin)
dr=d.get('droplet',{})
pub=next((n['ip_address'] for n in dr.get('networks',{}).get('v4',[]) if n['type']=='public'), '')
pub_v6=next((n['ip_address'] for n in dr.get('networks',{}).get('v6',[]) if n['type']=='public'), '')
out={
  'name': dr.get('name'),
  'id': dr.get('id'),
  'region': dr.get('region',{}).get('slug'),
  'size': dr.get('size_slug'),
  'image': dr.get('image',{}).get('slug'),
  'status': dr.get('status'),
  'public_ipv4': pub,
  'public_ipv6': pub_v6,
  'tags': dr.get('tags',[]),
  'created_at': dr.get('created_at'),
}
print(json.dumps(out, indent=2))
" > "$OUT_MANIFEST"

chmod 600 "$OUT_MANIFEST"
say "manifest written to $OUT_MANIFEST (mode 600)"
cat "$OUT_MANIFEST"

cat <<EOF

========================================================================
Next steps (once SSH is up on the droplet - usually 60-120s):

  1. Verify SSH:
       ssh -i ~/.ssh/datachain_rope_id_rsa root@${PUB_IP} 'hostnamectl'

  2. Add ~/.ssh/config alias (recommended):
       Host rope-testnet-1
         HostName ${PUB_IP}
         User root
         IdentityFile ~/.ssh/datachain_rope_id_rsa
         StrictHostKeyChecking accept-new

  3. Bootstrap the box (Reth + rope-testnet-node + faucet + nginx):
       ssh rope-testnet-1  # then follow docs/design/rope-testnet-writer-facade.md §5

  4. Point testnet DNS to ${PUB_IP} at Gandi:
       testnet.erpc.datachain.network  A  ${PUB_IP}  TTL 300
       faucet.datachain.network        A  ${PUB_IP}  TTL 300
       (testnet.dcscan.io stays where it is until we're ready to migrate the explorer.)

========================================================================
EOF
