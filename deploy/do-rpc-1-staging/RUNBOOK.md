# DO-rpc-1 ws.datachain.network vhost fix — DEPLOY RUNBOOK

**Target host:** `datachain-rpc-1` (`157.230.18.45`, DigitalOcean fra1)
**Staged:** 2026-08-12 (Rope agent)
**Status:** CODE-READY, NOT DEPLOYED. Deployment is operator-gated.
**Related:** §15 handover (BLUE ws.datachain.network fix, 2026-08-11).

---

## 0. What this fixes

Prior to this change, `dig +short ws.datachain.network` returns `92.243.26.189`
only (single-A). During any BLUE outage the DNS failover watcher on this box
promotes `erpc.datachain.network`, `erpc.rope.network`, and `ws.rope.network`
to `157.230.18.45` but **leaves `ws.datachain.network` pinned to BLUE**, so
every WebSocket client hitting the datachain.network hostname sees a 90-second
handshake timeout for the entire BLUE-down window.

The reason `ws.datachain.network` was excluded from the multi-A pool: the DO-rpc-1
vhost for `ws.datachain.network` proxies to `rope_rpc_edge` (port 8545, HTTP
JSON-RPC upstream). A WebSocket upgrade request sent to port 8545 gets a
`-32700 Parse error` back and the handshake never completes. Adding it to
DNS round-robin without fixing the vhost would just move the 90-second timeout
onto a different host.

This staging directory fixes both halves in one deploy:

1. **`datachain.network.conf`** — new `upstream rope_ws_local` block pointing
   at `127.0.0.1:8546` (the local rope-node WebSocket listener); WS vhost
   `proxy_pass` changed from `rope_rpc_edge` → `rope_ws_local`. Mirrors BLUE's
   §15 fix bit-for-bit, adjusted for DO-rpc-1's native-nginx layout
   (`127.0.0.1:8546` here vs BLUE's `host.docker.internal:8546` because BLUE
   runs nginx inside `rope-nginx` docker container).

2. **`dns-watcher-env.patch`** — env change on
   `/etc/datachain-rope-dns-failover.env` adding `ws:datachain.network` to
   `ROPE_DNS_NAMES`. After this, the DNS watcher promotes all 4 A records in
   lockstep during a BLUE outage and reverts all 4 when BLUE recovers.

---

## 1. Pre-flight checks

Run these on the operator's laptop or any box with SSH access to DO-rpc-1.

```bash
# 1.1  rope-node MUST be listening on port 8546 (WS) on 127.0.0.1
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 \
    'sudo ss -ltn "( sport = :8546 )" 2>&1'
# Expected: LISTEN on 127.0.0.1:8546 (or 0.0.0.0:8546, per §15 posture).
# If empty: rope-node is NOT running the WS listener on this node.
# Stop here — check rope-production.toml for ws_addr, restart datachain-rope.service.

# 1.2  Current DNS watcher env
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 \
    'sudo grep ROPE_DNS_NAMES /etc/datachain-rope-dns-failover.env'
# Expected: ROPE_DNS_NAMES="erpc erpc:rope.network ws:rope.network"

# 1.3  Current ws.datachain.network vhost target (should be the buggy one)
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 \
    'sudo grep -A2 "server_name ws.datachain.network" /etc/nginx/conf.d/datachain.network.conf | grep proxy_pass'
# Expected: proxy_pass http://rope_rpc_edge;   (this is the bug we're fixing)

# 1.4  BLUE handshake should currently succeed (staging box for the fix
#      does NOT need to break BLUE; we are only expanding the pool)
curl -sSI --resolve ws.datachain.network:443:92.243.26.189 \
     -H "Upgrade: websocket" -H "Connection: upgrade" \
     -H "Sec-WebSocket-Version: 13" -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" \
     --max-time 5 https://ws.datachain.network/ | head -1
# Expected: HTTP/1.1 101 Switching Protocols
```

If any pre-flight check fails, stop and file the finding rather than
proceeding.

---

## 2. Deploy sequence (operator, on DO-rpc-1)

```bash
# ── PHASE A: install the corrected nginx conf ──────────────────────────
# Backups are timestamped so successive attempts don't overwrite each other.

BAK_TS=$(date -u +%Y%m%dT%H%M%SZ)

# 2A.1  Stage the new conf onto the box
scp -i ~/.ssh/datachain_rope_id_rsa \
    /path/to/datachain-rope/deploy/do-rpc-1-staging/datachain.network.conf \
    root@157.230.18.45:/tmp/datachain.network.conf.staged

# 2A.2  Backup current, install new, syntax-check
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 "
    sudo cp /etc/nginx/conf.d/datachain.network.conf \
            /etc/nginx/conf.d/datachain.network.conf.bak-pre-ws-fix-${BAK_TS}
    sudo cp /tmp/datachain.network.conf.staged \
            /etc/nginx/conf.d/datachain.network.conf
    sudo nginx -t 2>&1
"
# Expected: 'nginx: configuration file /etc/nginx/nginx.conf test is successful'
# If syntax fails: sudo cp /etc/nginx/conf.d/datachain.network.conf.bak-pre-ws-fix-${BAK_TS} back and stop.

# 2A.3  Graceful reload (does NOT drop in-progress connections)
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 'sudo nginx -s reload'

# 2A.4  Verify from operator laptop: WS handshake against DO-rpc-1 now works
curl -sSI --resolve ws.datachain.network:443:157.230.18.45 \
     -H "Upgrade: websocket" -H "Connection: upgrade" \
     -H "Sec-WebSocket-Version: 13" -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" \
     --max-time 5 https://ws.datachain.network/ | head -1
# Expected: HTTP/1.1 101 Switching Protocols
# Was:      (times out at 5 s under curl, at 90 s under real WS client)

# ── PHASE B: activate the DNS watcher for ws.datachain.network ─────────

# 2B.1  Backup + patch env file
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 "
    sudo cp /etc/datachain-rope-dns-failover.env \
            /etc/datachain-rope-dns-failover.env.bak-pre-ws-datachain-${BAK_TS}
    sudo sed -i 's|^ROPE_DNS_NAMES=.*|ROPE_DNS_NAMES=\"erpc erpc:rope.network ws:rope.network ws:datachain.network\"|' \
        /etc/datachain-rope-dns-failover.env
    sudo grep ^ROPE_DNS_NAMES /etc/datachain-rope-dns-failover.env
"
# Expected: ROPE_DNS_NAMES="erpc erpc:rope.network ws:rope.network ws:datachain.network"

# 2B.2  Wait for the next watcher tick (≤ 30 s) — it auto-reads the env file
sleep 35
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 \
    'sudo tail -20 /var/log/erpc-dns-failover.log | grep -E "ws:datachain|heartbeat"'
# Expected: at least one "heartbeat ok target=blue names=… ws:datachain.network" line
# indicating the watcher picked up the new name without a failover event.
```

**Optional third A record for `ws.datachain.network`:**
The DNS watcher only alters records during a failure; it does not proactively
add DO-rpc-1 to the healthy-BLUE A record pool. If you want round-robin between
BLUE and DO-rpc-1 during normal healthy operation (matching how
`erpc.datachain.network` works today), open the Gandi API and set:

```
PUT /v5/livedns/domains/datachain.network/records/ws/A
{ "rrset_values": ["92.243.26.189", "157.230.18.45"], "rrset_ttl": 300 }
```

The single-A posture set in §15.4 of the dcscan handover was because
DO-rpc-1 was routing WS to port 8545 and would 90-second-timeout every
round-robin hit. Now that Phase A above fixes that, multi-A is safe.
Alternatively leave single-A and rely on DNS failover only during outages —
still a strict improvement over today's single-point-of-failure.

---

## 3. Post-deploy verification (operator laptop)

```bash
# 3.1  DO-rpc-1 WS handshake works (fresh connection)
curl -sSI --resolve ws.datachain.network:443:157.230.18.45 \
     -H "Upgrade: websocket" -H "Connection: upgrade" \
     -H "Sec-WebSocket-Version: 13" -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" \
     --max-time 5 https://ws.datachain.network/ | grep -i "^HTTP"
# Expect: HTTP/1.1 101 Switching Protocols

# 3.2  BLUE WS handshake unchanged
curl -sSI --resolve ws.datachain.network:443:92.243.26.189 \
     -H "Upgrade: websocket" -H "Connection: upgrade" \
     -H "Sec-WebSocket-Version: 13" -H "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==" \
     --max-time 5 https://ws.datachain.network/ | grep -i "^HTTP"
# Expect: HTTP/1.1 101 Switching Protocols

# 3.3  eth_blockNumber via WS on DO-rpc-1 (full end-to-end)
python3 - <<'PY'
import asyncio, json
try:
    import websockets
except Exception:
    print("SKIP: pip install websockets first")
    raise SystemExit(0)

async def main():
    url = "wss://ws.datachain.network/"
    # Manual --resolve equivalent: connect to the DO-rpc-1 IP but keep the
    # SNI + Host as ws.datachain.network so the cert matches.
    ssl_ctx = __import__("ssl").create_default_context()
    async with websockets.connect(url, ssl=ssl_ctx, server_hostname="ws.datachain.network",
                                   uri=url, extra_headers=[("Host", "ws.datachain.network")]) as ws:
        await ws.send(json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}))
        print(await ws.recv())
asyncio.run(main())
PY

# 3.4  DNS watcher heartbeat now covers 4 names
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 \
    'sudo tail -1 /var/log/erpc-dns-failover.log'
# Expect: names=erpc erpc:rope.network ws:rope.network ws:datachain.network
```

---

## 4. Rollback (single command per phase, either or both)

### 4A. Rollback nginx conf

```bash
BAK_TS=<the timestamp you used in step 2A.2>
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 "
    sudo cp /etc/nginx/conf.d/datachain.network.conf.bak-pre-ws-fix-${BAK_TS} \
            /etc/nginx/conf.d/datachain.network.conf
    sudo nginx -s reload
"
```

Effect: WS vhost on DO-rpc-1 goes back to the pre-fix state (proxies to
`rope_rpc_edge`, so any WS handshake attempts to this node will 90-second-
timeout again — which is exactly the situation this fix was written to
avoid). Only roll back if the fix itself is somehow breaking BLUE, which it
should not (adding a new upstream + changing one `proxy_pass` line on a
different vhost cannot affect BLUE).

### 4B. Rollback DNS watcher env

```bash
BAK_TS=<the timestamp you used in step 2B.1>
ssh -i ~/.ssh/datachain_rope_id_rsa root@157.230.18.45 \
    "sudo cp /etc/datachain-rope-dns-failover.env.bak-pre-ws-datachain-${BAK_TS} \
             /etc/datachain-rope-dns-failover.env"
```

Effect: watcher stops managing `ws.datachain.network` on the next tick.
The DNS record itself is unaffected — if the watcher had already promoted
it to DO-rpc-1 as part of a BLUE outage response, `dig` will still show
`157.230.18.45` until you manually revert via the Gandi API.

---

## 5. What this does NOT do

- Does not touch BLUE. BLUE's ws.datachain.network vhost was fixed in §15
  of the dcscan handover on 2026-08-11 and is not modified here.
- Does not add DO-rpc-2 as a third WS endpoint. That is a separate ops task
  (`n3_do_rpc2_vhost` in the dcscan handover; staging in
  `deploy/do-rpc-2-staging/`), tracked independently.
- Does not add a certbot deploy hook to auto-rsync `ws.datachain.network`
  cert renewals from BLUE. That is a P3 follow-up (see §15.6 in the dcscan
  handover). DO-rpc-1 already has a copy of the cert (valid through the
  next LE renewal window); we're relying on manual coordination for now.

---

## 6. Blast radius / concurrency

- The nginx graceful reload (`nginx -s reload`) in step 2A.3 does NOT drop
  in-progress connections. Existing HTTP JSON-RPC connections and WS
  subscriptions on DO-rpc-1 are unaffected.
- The env-file patch in step 2B.1 is picked up on the next 30-second
  watcher tick without any restart. No coordination window needed.
- Both phases are idempotent: running phase A twice is a no-op; running
  phase B twice writes the same env line twice.
- Neither phase touches any writer state, RocksDB, or the rope-node
  process itself. Zero risk of data loss or chain divergence.
