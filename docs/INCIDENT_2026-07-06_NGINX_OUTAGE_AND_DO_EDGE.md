# Incident record — public sites down (2026-07-04 → 2026-07-06) + DO edge failover

## Summary

Between 2026-07-04T08:33Z and 2026-07-06T12:36Z, every public HTTPS surface
(datachain.network, dcscan.io, erpc/ws.datachain.network, agents.*) was
unreachable. **The chain never stopped**: BLUE's rope-node, Reth, and
dc-explorer ran the whole time (47 days uptime), GREEN (121 days) and both
DO followers kept syncing every 15 minutes.

## Root cause

The `rope-nginx` Docker container on BLUE — the **only** public TLS entry
point for every domain — was manually stopped on Jul 4 at 08:33:57 UTC
(`docker inspect`: `hasBeenManuallyStopped=true`, exit code 0; dockerd
journal: "restart canceled ... hasBeenManuallyStopped=true"). The
`unless-stopped` restart policy deliberately honors manual stops, so the
container stayed down across the weekend. No auth-log session was captured
in the same minute; the stop most plausibly came from a stale operator
shell/session rather than an intrusion — ports 80/443 simply went silent,
nothing else on the box was touched.

## Why DigitalOcean did not "take the relay"

Failover existed only **inside** BLUE's nginx (`upstream digitalocean_rpc`:
BLUE → GREEN → DO-1 → DO-2). That protects against a *backend* failure, not
against the death of nginx itself. DNS for every domain points solely at
92.243.26.189, and the DO followers only exposed raw RPC on 8545 firewalled
to the Gandi IPs — no 80/443, no TLS certs, no static sites. With nginx down,
nothing anywhere could answer the public hostnames.

## Fixes applied (2026-07-06)

1. **Service restored** — `docker start rope-nginx`; all domains back to 200.
2. **nginx watchdog** — `/opt/datachain-rope/scripts/nginx-watchdog.sh` on
   BLUE (cron `*/2min`): starts the container if it is not running, restarts
   it if 443 stops answering. A manual stop now self-heals within 2 minutes.
3. **Full public edge on DO rpc-1 (157.230.18.45)** — nginx installed with
   mirrored vhosts (`deploy/edge-do/conf.d/` in datachain-rope-v2), serving:
   - datachain.network + www (static, local copy)
   - dcscan.io + www + api.dcscan.io (static + local dc-explorer :3001)
   - erpc/ws.datachain.network (local rope-node :8545, BLUE + DO-2 as backups)
   - faucet/bridge.datachain.network (static)
   - agents/semantic-agent/compliance-agent.datachain.network (proxied to
     BLUE :9091/:9092 — the agent processes only run on BLUE)
   TLS certs + static HTML sync hourly from BLUE
   (`/opt/datachain-rope/scripts/edge-do-sync.sh`, cron :25) and the edge
   nginx reloads after each sync so LE renewals propagate.
   UFW + the `datachain-rope-firewall` DO cloud firewall now allow 80/443.
   Verified working with `curl --resolve <host>:443:157.230.18.45`.

## Remaining single point of failure: DNS (operator action required)

All A records still point only at BLUE. To make the DO edge actually receive
traffic when BLUE dies, add a **second A record** per hostname in the Gandi
DNS console (or provide a Gandi PAT so an agent can do it):

| Hostname | Keep | Add |
|---|---|---|
| datachain.network, www | 92.243.26.189 | 157.230.18.45 |
| erpc.datachain.network | 92.243.26.189 | 157.230.18.45 |
| ws.datachain.network | 92.243.26.189 | 157.230.18.45 |
| dcscan.io, www, api | 92.243.26.189 | 157.230.18.45 |
| faucet/bridge.datachain.network | 92.243.26.189 | 157.230.18.45 |
| agents/semantic-agent/compliance-agent | 92.243.26.189 | 157.230.18.45 |

Round-robin A records give browser/client-level failover (clients retry the
other address on connection failure). Lower TTL to 300s at the same time.
A managed health-checked DNS (e.g. DO Load Balancer + single A record, or a
health-checking DNS provider) is the longer-term upgrade path.

## Verification commands

```bash
# BLUE serving (normal path)
curl -s -o /dev/null -w '%{http_code}\n' https://dcscan.io/

# DO edge serving (failover path, forced)
curl -s -o /dev/null -w '%{http_code}\n' --resolve dcscan.io:443:157.230.18.45 https://dcscan.io/
curl -s --resolve erpc.datachain.network:443:157.230.18.45 \
  -X POST https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# watchdog log on BLUE
ssh rope-vps 'tail /var/log/nginx-watchdog.log'
```
