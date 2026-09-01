# DO-rpc-2 Nginx Vhost Mirror — Deployment Runbook

**Status:** STAGED (2026-08-12). Not deployed. This runbook is the
operator-facing recipe for turning DO-rpc-2 into a 3rd DNS-failover
target for `erpc.datachain.network` / `erpc.rope.network` /
`ws.datachain.network` / `ws.rope.network`.

**Prerequisites:** operator has SSH access to DO-rpc-2 as `root` via
`~/.ssh/datachain_rope_id_rsa` (per §7 canonical-agents handover +
§13.5.1 follow-up note).

---

## 1. What this drop closes

Per `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc`
§13.5.1 and §15.6 P2:

> **P2 - mirror the WS vhost on DO-rpc-1** (`167.172.106.174`). DO-rpc-2
> currently has no nginx vhost for anything — it's Reth+rope-node only.
> Adding it requires: install nginx, copy `datachain.network.conf` +
> `rope.network.conf` + certs. ~10 min.

Also closes §16.7 P3:

> **P3 - mirror the `/v1/fleet-status*` stub on DO-rpc-2** (currently
> DO-rpc-2 has no nginx vhost at all for `erpc.datachain.network`, per
> §13.5.1 follow-up). Would give 3-way failover coverage.

Both are code-side prepared here; deploy stays operator-gated because
Nginx install + cert deploy are destructive-adjacent (touches
system paths, requires port 80/443 free, and the LE cert copy uses
credentials only the operator has).

## 2. What lands on DO-rpc-2

| Path on DO-rpc-2 | Source in this repo |
|---|---|
| `/etc/nginx/conf.d/datachain.network.conf` | `deploy/do-rpc-2-staging/datachain.network.conf` |
| `/etc/nginx/conf.d/rope.network.conf` | `deploy/do-rpc-2-staging/rope.network.conf` |
| `/opt/datachain-rope/ssl/erpc.datachain.network/{fullchain,privkey}.pem` | rsync from BLUE (see §5.3) |
| `/opt/datachain-rope/ssl/ws.datachain.network/{fullchain,privkey}.pem` | rsync from BLUE |
| `/opt/datachain-rope/ssl/erpc.rope.network/{fullchain,privkey}.pem` | rsync from BLUE |
| `/opt/datachain-rope/ssl/ws.rope.network/{fullchain,privkey}.pem` | rsync from BLUE |
| `/opt/datachain-rope/ssl/rope.network/{fullchain,privkey}.pem` | rsync from BLUE |

**Ports opened on the DO-rpc-2 firewall:** 80/tcp + 443/tcp (public).

## 3. Pre-flight gates

Run these BEFORE step 1 of §5:

```bash
# a) SSH works
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 'uptime'

# b) rope-node + Reth healthy on DO-rpc-2 (nginx will proxy to them)
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 \
  'systemctl is-active datachain-rope reth-rope && \
   curl -sS -X POST http://127.0.0.1:8545 \
     -H "content-type: application/json" \
     -d "{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}"'

# c) BLUE reachable from DO-rpc-2 (backup upstream will use 92.243.26.189:8545)
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 \
  'nc -w 5 92.243.26.189 8545 </dev/null && echo BLUE-reachable'

# d) DO-rpc-1 reachable from DO-rpc-2 (backup upstream will use 157.230.18.45:8545)
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 \
  'nc -w 5 157.230.18.45 8545 </dev/null && echo DO1-reachable'

# e) Ports 80/443 free on DO-rpc-2
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 \
  'ss -ltn "( sport = :80 or sport = :443 )"'
# Expected: no output (both ports free)

# f) LE certs exist on BLUE (source of truth for cert copies)
ssh rope-vps 'sudo ls /opt/datachain-rope/ssl/{erpc,ws}.{datachain,rope}.network/{fullchain,privkey}.pem /opt/datachain-rope/ssl/rope.network/{fullchain,privkey}.pem'
```

If any gate fails, stop and diagnose. Common failures:

- `ss -ltn` shows another service on 80/443: it must be stopped first.
- `nc` to BLUE/DO1 fails: DO-rpc-2's UFW may block outbound to those IPs; add allow rules before proceeding.

## 4. Rollback plan

At any point during §5, the following removes all changes without
touching Reth / rope-node:

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 '
  sudo systemctl stop nginx || true
  sudo rm -f /etc/nginx/conf.d/datachain.network.conf /etc/nginx/conf.d/rope.network.conf
  sudo systemctl disable nginx || true
'
```

DNS: no changes required to roll back. If §7 was executed (adding
DO-rpc-2 to `ROPE_DNS_NAMES` on the watcher), also do:

```bash
ssh datachain-rpc-1 '
  sudo sed -i "s|^ROPE_DNS_NAMES=.*|ROPE_DNS_NAMES=\"erpc erpc:rope.network ws:rope.network\"|" \
    /etc/datachain-rope-dns-failover.env
'
```

## 5. Deploy sequence

### 5.1 Install nginx (~30 s)

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 '
  DEBIAN_FRONTEND=noninteractive apt-get update && \
  DEBIAN_FRONTEND=noninteractive apt-get install -y nginx
'
```

The default distro nginx package is fine — no need to install
`nginx-full` unless we later want extra modules (currently we don't).

### 5.2 Remove default site + drop staging configs

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 '
  # Nuke the default site so our vhosts have exclusive :80/:443
  rm -f /etc/nginx/sites-enabled/default
  # Ensure conf.d/ exists (it does on ubuntu by default; belt+suspenders)
  mkdir -p /etc/nginx/conf.d
  mkdir -p /opt/datachain-rope/ssl
'

# Sync the two vhost files from local
rsync -av -e "ssh -i ~/.ssh/datachain_rope_id_rsa" \
  /Users/kazealphonseonguene/Downloads/DATACHAIN\ ROPE/datachain-rope/deploy/do-rpc-2-staging/datachain.network.conf \
  root@167.172.106.174:/etc/nginx/conf.d/datachain.network.conf

rsync -av -e "ssh -i ~/.ssh/datachain_rope_id_rsa" \
  /Users/kazealphonseonguene/Downloads/DATACHAIN\ ROPE/datachain-rope/deploy/do-rpc-2-staging/rope.network.conf \
  root@167.172.106.174:/etc/nginx/conf.d/rope.network.conf
```

### 5.3 Copy LE certs from BLUE

Certs live under `/opt/datachain-rope/ssl/<hostname>/` on BLUE. We
pull them locally through the operator's laptop (rsync -e ssh doesn't
compose two remote endpoints), then push to DO-rpc-2:

```bash
LOCAL_STAGE=/tmp/rope-le-certs-$(date -u +%Y%m%dT%H%M%SZ)
mkdir -p "$LOCAL_STAGE"

for h in erpc.datachain.network ws.datachain.network erpc.rope.network ws.rope.network rope.network; do
  mkdir -p "$LOCAL_STAGE/$h"
  rsync -av --rsync-path='sudo rsync' \
    "rope-vps:/opt/datachain-rope/ssl/$h/fullchain.pem" \
    "rope-vps:/opt/datachain-rope/ssl/$h/privkey.pem" \
    "$LOCAL_STAGE/$h/"
done

# Push all cert bundles to DO-rpc-2 in one shot
rsync -av -e "ssh -i ~/.ssh/datachain_rope_id_rsa" \
  "$LOCAL_STAGE/" root@167.172.106.174:/opt/datachain-rope/ssl/

# Verify the copy landed with correct perms (0600 keys, 0644 chains)
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 '
  chmod 0600 /opt/datachain-rope/ssl/*/privkey.pem
  chmod 0644 /opt/datachain-rope/ssl/*/fullchain.pem
  ls -la /opt/datachain-rope/ssl/*/*.pem
'

# Clean up local stage
rm -rf "$LOCAL_STAGE"
```

**IMPORTANT:** the operator MUST also add a certbot deploy-hook on BLUE
that rsyncs renewed certs to DO-rpc-2 (and DO-rpc-1). Without it, the
certs on the DO nodes go stale at the next LE renewal (~60 days).
Deferred to §9 — this is a separate follow-up ticket.

### 5.4 Firewall (open 80/443, keep 8545/8595 tight)

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 '
  ufw allow 80/tcp
  ufw allow 443/tcp
  # Confirm the JSON-RPC + Reth ports stay restricted to the four
  # in-fleet peers per digitalocean-third-blue-green-slot.mdc §Firewall
  ufw status | grep -E "8545|8595"
'
```

### 5.5 Nginx syntax check + first start

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 '
  nginx -t && systemctl enable --now nginx && systemctl is-active nginx
'
# Expected: "nginx: the configuration file /etc/nginx/nginx.conf syntax is ok"
#           "nginx: configuration file /etc/nginx/nginx.conf test is successful"
#           "active"
```

## 6. Post-deploy verification (from operator laptop)

None of these require DNS to be failed over — we use `--resolve` to
address DO-rpc-2 directly for each hostname.

```bash
DO2=167.172.106.174

# 6.1 Local healthz (honest, not failover-covered)
curl -sSk --resolve erpc.datachain.network:443:$DO2 \
  https://erpc.datachain.network/healthz \
  -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'
# Expected: {"jsonrpc":"2.0","result":"0x...","id":1}

# 6.2 fleet-status stub returns the failover marker
curl -sSk --resolve erpc.datachain.network:443:$DO2 \
  https://erpc.datachain.network/v1/fleet-status
# Expected: HTTP 503, body {"status":"failover_no_fleet_status","served_by":"do-rpc-2",...}

# 6.3 CORS preflight succeeds
curl -sSk --resolve erpc.datachain.network:443:$DO2 -X OPTIONS -I \
  https://erpc.datachain.network/
# Expected: HTTP/2 204, Access-Control-Allow-* headers

# 6.4 erpc.rope.network JSON-RPC works
curl -sSk --resolve erpc.rope.network:443:$DO2 \
  -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
  https://erpc.rope.network/
# Expected: HTTP 200, {"jsonrpc":"2.0","result":"0x...","id":1}

# 6.5 rope.network apex redirects to datachain.network
curl -sSkI --resolve rope.network:443:$DO2 https://rope.network/
# Expected: HTTP/2 301, location: https://datachain.network/

# 6.6 WSS handshake reaches local rope-node
python3 - <<'PY'
import socket, ssl
ctx = ssl.create_default_context()
ctx.check_hostname = False
ctx.verify_mode = ssl.CERT_NONE
with socket.create_connection(("167.172.106.174", 443), timeout=10) as sock:
    with ctx.wrap_socket(sock, server_hostname="ws.datachain.network") as s:
        req = b"GET / HTTP/1.1\r\nHost: ws.datachain.network\r\nUpgrade: websocket\r\n" \
              b"Connection: Upgrade\r\nSec-WebSocket-Version: 13\r\n" \
              b"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        s.send(req)
        print(s.recv(1024).decode(errors="replace"))
PY
# Expected: "HTTP/1.1 101 Switching Protocols"

# 6.7 V11 destructive-gate proof via the failover node
curl -sSk --resolve erpc.datachain.network:443:$DO2 \
  -X POST -H 'content-type: application/json' -H 'X-Rope-Internal-Token: forge' \
  -d '{"jsonrpc":"2.0","method":"rope_createPersonalLedger","params":["0x1111111111111111111111111111111111111111"],"id":1}' \
  https://erpc.datachain.network/
# Expected: {"error":{"code":-32401,"message":"Method denied on public listener; see SECURITY_AUDIT_2026-06-11.md"},...}
# Header strip must have removed X-Rope-Internal-Token before it reached rope-node.
```

## 7. Add DO-rpc-2 to the DNS failover watcher (OPTIONAL, ops-gated)

After §6 is green for 24 h, add DO-rpc-2 as a candidate promote target
on the DNS watcher. Currently the watcher on DO-rpc-1 only knows about
BLUE ↔ DO-rpc-1. Extending to DO-rpc-2 is a script-side change (out
of scope for this drop) — file as follow-up ticket §9.4.

For now, the operator can MANUALLY promote to DO-rpc-2 by editing the
`erpc.datachain.network` A record in Gandi LiveDNS to point at
`167.172.106.174` (already validated by §6 that the box serves the
right JSON). Reverting is one edit back to `92.243.26.189` (BLUE).

## 8. Live rollback (single command, after step 6 passes)

If any test in §6 fails or DO-rpc-2's nginx starts causing real user
pain, run:

```bash
ssh -i ~/.ssh/datachain_rope_id_rsa root@167.172.106.174 '
  systemctl stop nginx
  systemctl disable nginx
'
```

Public DNS never resolved to DO-rpc-2 (unless §7 was executed), so
users are unaffected. Reth + rope-node keep running.

## 9. Follow-ups (not blocking)

| # | Item | Effort |
|---|---|---|
| 9.1 | **Certbot deploy hook on BLUE** that rsyncs renewed certs to BOTH DO-rpc-1 AND DO-rpc-2 (currently DO-rpc-1 has the same gap). ~30 min: write hook, test with `certbot renew --dry-run --deploy-hook`. | 30 min |
| 9.2 | **Extend the DNS failover watcher on DO-rpc-1** to include DO-rpc-2 as a promotion target (round-robin, health-checked). ~1 h: script edit + tests. | 1 h |
| 9.3 | **Cross-region secondary watcher on DO-rpc-2** so a DO-rpc-1 outage does not disable DNS failover entirely. Requires a small coordination layer (shared state file or Gandi API idempotency). | 2-3 h |
| 9.4 | **Add DO-rpc-2 IP** to any allowlists / firewall rules on BLUE, DCSwap, Tanastok, Datawallet+ that currently trust only DO-rpc-1. | 30 min |
| 9.5 | **Reflect in `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc` §26** once deployed, updating the DO-rpc-2 status from "no vhost" to "3-way failover ready". | 15 min |
