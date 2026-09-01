# BLUE migration - pre-cutover checkpoint (2026-08-23)

**Purpose:** state-of-the-migration snapshot the operator can read to decide when to open the maintenance window. All destructive steps (DNS flips, service stops, nginx reload) are staged but NOT executed. Every "staged" artefact is reversible with a one-liner.

**Live snapshot:** 2026-08-23T~12:35Z. IPFS bulk rsync in progress (~40% done, ETA ~10 min).

---

## 1. What is DONE (safe, reversible, verified)

| # | Artefact | Where | Reversibility |
|---|---|---|---|
| 1 | New droplet `rope-vps-lon1` (s-8vcpu-32gb-amd, 400 GB SSD, London `lon1`) | DigitalOcean droplet id `594545209`, direct IP `167.172.51.76`, Floating IP `159.65.208.206` attached | `doctl compute droplet delete 594545209` |
| 2 | Cloud-init bootstrap (Docker 29.7.2, Kubo 0.33.2, certbot 2.9.0 + Gandi plugin, Rust 1.98.0, UFW active with 22/80/443/4001/30303, sysctl P0-A2 applied) | Verified live on new BLUE | Reprovision |
| 3 | SSH keys: BLUE ubuntu, operator local `DCRope_key`, and `datachain-rope` RSA all in `/home/ubuntu/.ssh/authorized_keys` and `/root/.ssh/authorized_keys` | Verified `ssh -i ~/.ssh/DCRope_key root@159.65.208.206` and `ssh -i ~/.ssh/DCRope_key ubuntu@159.65.208.206` both work | Remove keys |
| 4 | Reth `static_files` (5.7 GB, immutable) rsynced to `/opt/datachain-rope/reth/data/static_files/` on new BLUE | Verified: `du -sh` matches old BLUE within 1 MB | `rm -rf` |
| 5 | Binaries + deploy tree: `rope`, `dc-explorer`, `rope-idp`, `semantic-agent` + `deploy/nginx/` + `deploy/systemd/` staged at `/home/ubuntu/rope-src-staging/` | Verified: MD5 sums match old BLUE | `rm -rf` |
| 6 | `/opt/datachain-rope/{code,ssl}` staged at `/etc/rope-migration/opt/datachain-rope/{code,ssl}` | Verified: `code/init-db/*.sql` present for postgres bootstrap | `rm -rf` |
| 7 | 13 systemd drop-ins staged at `/etc/rope-migration/etc/systemd/system/` including new locally-developed `71-memory-swap-post-upgrade.conf` and `72-memory-circuit-breaker.conf` | Verified | Do not `daemon-reload` |
| 8 | Let's Encrypt certs (7 domains: agents/api.dcscan/console/erpc.datachain/erpc.rope/id/naturaproof-redirect) already live at `/etc/letsencrypt/live/` on new BLUE via cloud-init | Verified: 7/7 privkey and fullchain match | Certbot has its own state; delete `/etc/letsencrypt/` if needed |
| 9 | `/etc/rope-migration/etc/systemd/system/` cron captures + all 40+ systemd unit files from old BLUE | Verified | Do not enable |
| 10 | Docker images `nginx:alpine`, `postgres:16-alpine`, `redis:7-alpine` pre-pulled on new BLUE | Verified `docker images` on new BLUE | `docker rmi` |
| 11 | DO-rpc-1 nginx patches staged at `/root/rope-migration-staging/` on 157.230.18.45 (2 patched files: `datachain.network.conf.new-blue-patch`, `agents.datachain.network.conf.new-blue-patch`; pre-cutover backups also saved) | Verified `diff` shows only `92.243.26.189` -> `159.65.208.206` substitution + comment updates | Restore from `.pre-cutover-bak` |
| 12 | DO-rpc-2 nginx patch staged at `/root/rope-migration-staging/` on 167.172.106.174 (1 patched file: `datachain.network.conf.new-blue-patch`) | Verified `diff` clean | Restore from `.pre-cutover-bak` |

## 2. What is IN PROGRESS

| # | Task | Status | ETA |
|---|---|---|---|
| 13 | IPFS `blocks/` bulk rsync (immutable content-addressed blocks, ~5.6 GB) from old BLUE to new BLUE | ~40% complete at 12:35Z (2.1 GB / 5.6 GB). Running under `nice`/`ionice`/`bwlimit=40000` on old BLUE, PID 2201593 | ~10-15 more min |

## 3. What NEEDS OPERATOR AUTHORIZATION (destructive, live-production)

Two categories: (a) TTL reduction (can be done ≥3 hr before cutover, no downtime, only reduces future caching duration); (b) actual cutover (opens maintenance window).

### 3.1 Category (a) - TTL reduction on `dcscan.io` and `api.dcscan.io`

**Why now, not at cutover:** `dcscan.io` and `api.dcscan.io` currently have TTL=10800 (3 hr). If we flip these records during the cutover window, cached resolvers will keep serving the old IP for up to 3 hr. Lowering TTL to 300 now costs zero and gives us a fast cutover later. If cutover is delayed >1 week, we can raise TTL back.

**Records to lower (via Gandi API v5 LiveDNS):**

| Domain | Type | Current TTL | Target TTL |
|---|---|---|---|
| `dcscan.io` | A | 10800 | 300 |
| `api.dcscan.io` | A | 10800 | 300 |

**API call template (uses `GANDI_API_KEY` in `.env`, verified 2026-08-23T12:30Z):**

```bash
GANDI_KEY=$(grep GANDI_API_KEY .env | cut -d= -f2)
curl -sS -X PUT \
  -H "Authorization: Bearer $GANDI_KEY" \
  -H "Content-Type: application/json" \
  -d '{"rrset_values":["92.243.26.189"],"rrset_ttl":300}' \
  https://api.gandi.net/v5/livedns/domains/dcscan.io/records/@/A
# and again with .../api/A for api.dcscan.io
```

**Reversibility:** re-PUT with `"rrset_ttl":10800`.

**Blast radius:** zero. No routing change. Only future record caching duration.

**Recommendation:** authorize now so we do not block the maintenance window on TTL propagation.

### 3.2 Category (b) - the maintenance window itself

**Duration estimate:** ~25-40 min wall time. Writes blocked for the whole window. Reads may briefly 5xx during nginx reload on DO-rpc-1 and DO-rpc-2.

**Prerequisites (all done except IPFS):**
- All items 1-12 above staged
- IPFS rsync (item 13) completed
- TTL reduction (3.1) landed >= 3 hr before cutover

**Cutover phases (all commands staged as scripts; nothing runs until operator says "go"):**

**Phase A - stop-and-final-delta (~5 min on old BLUE, writes blocked from this point):**
1. Announce maintenance start (Slack/email if used)
2. Stop old BLUE services in dependency order:
   - `systemctl stop erpc-fleet-ha.timer erpc-fleet-ha.service` (self-heal loop off)
   - `systemctl stop semantic-agent oracle-agent insurance-agent validation-agent compliance-agent`
   - `systemctl stop dc-explorer` (dcscan/dcexplorer API server)
   - `systemctl stop datachain-rope` (rope-node sealer - THIS is when writes stop)
   - `systemctl stop ipfs`
   - `docker compose -f /opt/datachain-rope/code/docker-compose.yml down` (rope-nginx, rope-postgres, rope-redis)
3. Final delta rsync (small):
   - Reth `data/db/` and `data/rocksdb/` (writable state, small vs static_files)
   - IPFS `datastore/` and `config` (writable state)
   - `pg_dump dcscan > /tmp/dcscan-final.sql` on old BLUE, scp to new BLUE
4. Rsync `/etc/rope-migration/` -> `/etc/` on new BLUE (systemd drop-ins + unit files)
5. Rsync `/etc/rope-migration/opt/datachain-rope/{code,ssl}` -> `/opt/datachain-rope/` on new BLUE
6. Rsync `/home/ubuntu/rope-src-staging/` -> `/home/ubuntu/datachain-rope/` on new BLUE
7. `chown -R ubuntu:ubuntu` on new BLUE for all datachain paths

**Phase B - bring new BLUE up (~5 min):**
1. `systemctl daemon-reload` on new BLUE
2. Start services in dependency order:
   - `systemctl start ipfs`
   - `systemctl start datachain-rope` (rope-node) - **verify block production resumes**
   - `docker compose -f /opt/datachain-rope/code/docker-compose.yml up -d postgres redis` - **restore pg_dump: `docker exec -i rope-postgres psql -U dcscan dcscan < /tmp/dcscan-final.sql`**
   - `systemctl start dc-explorer` (needs postgres)
   - `systemctl start semantic-agent oracle-agent insurance-agent validation-agent compliance-agent`
   - `docker compose ... up -d nginx` (last, needs TLS certs)
   - `systemctl start erpc-fleet-ha.timer` (last, needs everything)
3. Verify new BLUE health:
   - `curl http://127.0.0.1:8545 -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'` returns fresh block
   - `curl -k https://159.65.208.206/api/v1/stats` returns dcscan stats
   - `journalctl -u datachain-rope -n 50` shows no errors

**Phase C - flip the world (~5 min):**
1. **Apply DO-rpc-1 nginx patches** (155.230.18.45):
   - `cp /root/rope-migration-staging/datachain.network.conf.new-blue-patch /etc/nginx/conf.d/datachain.network.conf`
   - `cp /root/rope-migration-staging/agents.datachain.network.conf.new-blue-patch /etc/nginx/conf.d/agents.datachain.network.conf`
   - `nginx -t && nginx -s reload`
2. **Apply DO-rpc-2 nginx patch** (167.172.106.174):
   - `cp /root/rope-migration-staging/datachain.network.conf.new-blue-patch /etc/nginx/conf.d/datachain.network.conf`
   - `nginx -t && nginx -s reload`
3. **Flip DNS A records via Gandi API** (9 records total, in parallel):
   - `dcscan.io` A -> 159.65.208.206
   - `api.dcscan.io` A -> 159.65.208.206
   - `agents.datachain.network` A -> 159.65.208.206
   - `console.datachain.network` A -> 159.65.208.206
   - `id.datachain.network` A -> 159.65.208.206
   - `semantic-agent.datachain.network` A -> 159.65.208.206
   - `naturaproof.io` A -> 159.65.208.206 (in naturaproof.io zone)
   - `naturaproof.net` A -> 159.65.208.206 (in naturaproof.net zone)
   - `naturaproof.org` A -> 159.65.208.206 (in naturaproof.org zone)
4. Note: `erpc.datachain.network`, `ws.datachain.network`, `erpc.rope.network` already point to DO-rpc-1 (157.230.18.45); they need NO DNS change - only the DO-rpc-1/2 nginx upstream flip in step 1/2.

**Phase D - verify (~5 min):**
1. From an external client:
   - `curl https://erpc.datachain.network -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'` - should return fresh block
   - `curl https://dcscan.io/api/v1/stats` - should return live stats
   - `curl https://agents.datachain.network/v1/health` - should be 200
   - Repeat for id, console, semantic-agent, naturaproof.io/.net/.org
2. From tanastok/dcswap/careaway: verify their RPC probes and mesh peers see the new BLUE
3. Announce cutover complete

**Phase E - cross-project peer updates (~5-10 min, non-blocking):**
1. Any code with hardcoded `92.243.26.189` in DCSwap/Tanastok/CareAway/Datawallet+ configs -> replace with `159.65.208.206` (or better: with `erpc.datachain.network` if applicable)
2. Handovers: drop `handover-blue-migrated-lon1-2026-08-23.mdc` into `dcswap/.cursor/rules/`, `tanastok/.cursor/rules/`, etc.

## 4. Fallback plan (if new BLUE fails within 7-14 days)

Old BLUE at `92.243.26.189` (Gandi Paris) remains intact and reachable via SSH `-p 41722` for **7-14 days after cutover** (per operator D3 decision). If new BLUE catastrophically fails:

1. Revert DO-rpc-1/2 nginx patches (restore `.pre-cutover-bak` files, reload)
2. Revert DNS A records to `92.243.26.189` via Gandi API
3. Restart services on old BLUE in dependency order
4. Investigate failure on new BLUE offline

Estimated rollback time: 15-30 min. Reversibility is preserved because we never destroy old BLUE state.

## 5. What is being intentionally NOT touched

- `rope-offload-01` (fra1 s-2vcpu-4gb) - operator picked `offload_destroy` in D2. To be destroyed AFTER successful cutover + 7-day soak. Continues running until then (cost: ~$0.06/day, worth it as insurance).
- GREEN (Gandi `92.243.25.119`), DO-rpc-1 (157.230.18.45), DO-rpc-2 (167.172.106.174) - unchanged as attesters/read failover. Their code is untouched.
- Old BLUE Reth data - remains on disk on old BLUE. Do not `rm -rf` until 7-14 day soak passes.

## 6. Post-cutover verification checklist (24-hr window)

- [ ] Block production continuous (rope-node emits blocks every 4-8 sec)
- [ ] fleet-status endpoint healthy from external network
- [ ] IPFS peer count grows (new BLUE re-establishes swarm)
- [ ] All 55 IPFS pins accessible via `ipfs cat`
- [ ] `dcscan.io` renders stats + latest transactions
- [ ] Agents (semantic/oracle/insurance/validation/compliance) all report Recent testimony anchors
- [ ] TLS certs (7 domains) all valid, certbot renewal timer active
- [ ] Cross-project peer status: DCSwap/Tanastok/CareAway CERBER mesh reports new BLUE reachable
- [ ] No 5xx spikes in DO-rpc-1/2 nginx logs
- [ ] Memory pressure: `PSI full avg60` < 5.0 sustained (P0-A2 delivery)
- [ ] Swap use: 0 (P0-A2 delivery when `71-post-upgrade` applied)

## 7. Cross-references

- `docs/BLUE_MIGRATION_TO_DO_LON1_RUNBOOK_2026-08-23.md` - the full migration runbook this checkpoint operationalizes
- `docs/P0_P1_P2_INTEGRATED_SEQUENCE_2026-08-23.md` - the plan that put us here
- `docs/A3_ALTERNATIVES_GANDI_QUOTA_BLOCK_2026-08-23.md` - why we are doing this at all
- `.cursor/rules/handover-blue-migration-to-do-lon1-2026-08-23.mdc` - concise handover to future agents
- `.cursor/rules/handover-p0-p1-p2-sequence-2026-08-23.mdc` - honest scope reminder

---

## 8. Status of the two gates

**Gate 1 (TTL reduction):** ready to fire on operator authorization. Blast radius zero. Recommend authorize now so cutover window is not gated on 3-hr DNS propagation.

**Gate 2 (maintenance window open):** ready in ~10-15 min once IPFS rsync finishes. Operator will be paged when IPFS is at 100%. Cutover itself takes ~25-40 min wall time; recommend a 60-min buffer to be safe.
