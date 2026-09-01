# BLUE Migration Runbook: Gandi (Paris SD6, 8 GB) -> DigitalOcean (lon1, s-8vcpu-32gb-amd)

**Author:** ROPE agent
**Date:** 2026-08-23
**Status:** DRAFT - operator review gate before provisioning ($168/mo commitment)
**Supersedes:** in-place Gandi 8 GB -> 16 GB resize (blocked at Gandi 20/20 core quota)
**Related docs:**
- `docs/A3_ALTERNATIVES_GANDI_QUOTA_BLOCK_2026-08-23.md` (option selection)
- `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` (superseded by this document)
- `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` (root cause)
- `docs/P0_P1_P2_INTEGRATED_SEQUENCE_2026-08-23.md` (A2 pre-upgrade already deployed on Gandi BLUE)

---

## 0. TL;DR + operator decisions required

### 0.1 What this migration does

Move BLUE (Datachain Rope sealer + writer) from Gandi VPS `92.243.26.189` to a new DigitalOcean droplet in London (`lon1`), routed via a DigitalOcean Floating IP that we can reassign for future migrations without another DNS cutover.

- **Target size:** `s-8vcpu-32gb-amd` at $168/mo (8 vCPU AMD EPYC, 32 GB RAM, 400 GB NVMe SSD, 10 TB egress)
- **Region:** `lon1` (London) - user-picked for geographic diversity from fra1 fleet
- **Public IP:** DigitalOcean Floating IP (reserved once, reassignable), NOT the droplet's default IP
- **Old BLUE:** kept alive for 7-14 days as read-only fallback (drain but do not decommission)
- **rope-offload-01 (fra1):** becomes redundant since user chose "migrate with IPFS in place". Recommend destroy after cutover ($21/mo savings) OR repurpose as future ipfs offload target if BLUE memory pressure returns

### 0.2 Blast radius

| Impact | Duration | Users affected |
|---|---|---|
| **Writes blocked** (`eth_sendRawTransaction`, destructive `rope_*`, txpool_*) | 60-120 min (maintenance window) | Every DCSwap swap, migration mint, governance vote submission, agent knot anchor emitter |
| **Reads degraded** (public erpc RPC + WebSockets) | 0-5 min per record during DNS propagation | Anyone hitting `erpc.datachain.network` / `ws.datachain.network` |
| **dcscan.io** (block explorer + supply endpoints) | 0-5 min | Public block explorer users |
| **id.datachain.network** (SSO) | 0-5 min | Datawallet+ SSO login sessions (existing tokens keep working, new logins fail during cutover) |
| **agents.datachain.network** (semantic + compliance) | 0-5 min | GDPR Art.17 clients, semantic search |
| **naturaproof.io** (redirect only) | 0-5 min | Marketing site fallback |
| **CERBER mesh coverage** (`cerber-rope` peer) | 60-120 min | DCSwap R13 pages during window - suppress before, resume after |

### 0.3 Cost impact

| Item | Before | During (2-4 weeks) | After |
|---|---|---|---|
| Gandi BLUE (VPS 8 GB, 4 core) | $40/mo est. | $40/mo (kept as fallback) | $0 (decommissioned) |
| DO new BLUE (s-8vcpu-32gb-amd) | $0 | $168/mo | $168/mo |
| DO Floating IP (attached to droplet) | $0 | $0 (free when attached) | $0 |
| DO rope-offload-01 (s-2vcpu-4gb) | $21/mo | $21/mo | $0-21/mo (see decision below) |
| **Total delta** | | +$149/mo overlap | +$128/mo steady state |

### 0.4 Operator decisions gating this runbook

Before I provision anything, three operator confirmations needed:

**D1. Certbot strategy for the 7 TLS certs:**
- Option A: **Copy `/etc/letsencrypt/` from old BLUE to new BLUE via rsync** (fastest, keeps cert continuity, but requires port 80 on new BLUE reachable BEFORE cutover so renewal works; DNS-01 is safer)
- Option B: **Re-issue via DNS-01 challenge on new BLUE** before cutover (safest, no dependency on port 80, but requires Gandi DNS API credentials in `/etc/letsencrypt/dns-gandi.ini` - need to check if we already have these)
- Option C: **Wait for HTTP-01 renewal after cutover** (cleanest but leaves TLS certs valid on OLD BLUE only until first renewal; if renewal fails, cert expires and every browser HTTPS request breaks)

**Recommended: Option B (DNS-01)** if Gandi API creds exist; else Option A. Certificate lifetimes are 90 days so we have time to migrate to DNS-01 on the new host.

**D2. Fate of rope-offload-01 (fra1 IPFS offload droplet, $21/mo):**
- Option X: **Destroy immediately after cutover** - IPFS stays on BLUE, offload droplet was never used
- Option Y: **Keep as future IPFS offload target** - $21/mo insurance policy if BLUE memory pressure returns after migration
- Option Z: **Repurpose as fra1 read-only follower** (rope-node + reth in follower mode, adds fra1 regional read failover)

**Recommended: Option X (destroy)** - simplest, saves $21/mo, easy to re-provision if needed later.

**D3. Old Gandi BLUE fallback window:**
- Option 1: **Fast decommission** (24-48h after cutover verify) - $40/mo savings faster, but no rollback if new BLUE has hidden issues
- Option 2: **Standard fallback** (7-14 days) - drain writes, keep as read-only observer, cancel Gandi after MTBF verified on new BLUE
- Option 3: **Long fallback** (30 days) - overkill unless we have concerns about lon1 network reliability

**Recommended: Option 2 (7-14 days)** - gives one MTBF cycle to prove new BLUE is stable before losing the fallback.

---

## 1. Migration overview

### 1.1 Phase timeline

| Phase | Wall time | Who acts | State on old BLUE | State on new BLUE |
|---|---|---|---|---|
| Phase 0: pre-flight | 30 min | ROPE agent + operator | writes accepted | not provisioned |
| Phase 1: provision | 10 min | ROPE agent | writes accepted | droplet booted, no services |
| Phase 2: bootstrap OS | 60 min | ROPE agent | writes accepted | packages installed, users created, UFW configured |
| Phase 3: pre-cutover rsync | 2-4 hours | ROPE agent (background) | writes accepted | Reth + IPFS + config on disk, services NOT started |
| Phase 4: TLS cert prep | 30 min | ROPE agent + operator D1 | certs valid | certs installed |
| Phase 5: peer pre-registration | 30 min | ROPE agent + downstream projects | peer pinned | new IP pre-announced but not primary |
| Phase 6: **MAINTENANCE WINDOW - writes blocked** | **60-120 min** | operator supervision | services stopped -> read-only | services started, writer active |
| Phase 7: DNS cutover | 5-30 min (TTL dependent) | ROPE agent | draining | primary |
| Phase 8: cross-project peer swap | 30 min | ROPE agent + DCSwap/Tanastok agents | drained | primary |
| Phase 9: post-cutover verify | 60 min | ROPE agent + operator | idle | primary, verified |
| Phase 10: fallback observation | 7-14 days | ROPE agent (monitor) | idle | steady state |
| Phase 11: old BLUE decommission | 15 min | operator | destroyed | primary |

Total operator time: ~4-6 hours across 2-3 days, plus 60-120 min supervised maintenance window.

### 1.2 What moves, what stays

**Moves to new BLUE:**
- All 17 systemd services (see §2.2 for full list)
- All 3 Docker containers (rope-nginx, rope-postgres, rope-redis)
- All 11 cron jobs (root + ubuntu)
- Reth EVM data (~11 GB)
- IPFS data + PeerID + 55 pins (~7.8 GB)
- rope-node ledger RocksDB (~few GB)
- Postgres data (size TBD in Phase 0)
- `/opt/datachain-rope/` source tree + config + scripts
- `/etc/letsencrypt/` TLS certs (Option A) OR re-issue via DNS-01 (Option B)
- SSH keys (`~/.ssh/`, `~ubuntu/.ssh/`)
- Cerber mesh identity + peer overlay (`/var/lib/cerber/`)
- Foundation Ed25519 signing keys (rope-node testimony)

**Stays on new BLUE identity:**
- Same rope-node NodeId (blake3 of ed25519 pubkey; ValidatorRegistry entry unchanged)
- Same IPFS PeerID (`12D3Koo...` hash of libp2p key; pins survive)
- Same TLS cert chain (Option A) OR fresh but for same domains (Option B)

**Changes:**
- Public IP (Gandi 92.243.26.189 -> DO Floating IP, TBA at Phase 1)
- Datacenter (Paris SD6 -> London LON1)
- Latency to Paris peers (was ~1 ms LAN -> ~15 ms cross-Channel)

**Redundant after cutover:**
- rope-offload-01 (fra1) - never populated with IPFS data; decommission per D2

### 1.3 Anti-patterns explicitly forbidden

- Do NOT stop the sealer before the final rsync (data loss)
- Do NOT delete old BLUE before Phase 10 verification (no rollback)
- Do NOT swap DNS before TLS certs are installed on new BLUE (HTTPS breaks)
- Do NOT provision without SSH key auth (password auth = compromise risk)
- Do NOT set Floating IP as primary before rope-node + reth healthy on new BLUE
- Do NOT deploy A2 71-post-upgrade.conf or B2 memory circuit breaker on old BLUE - they are for the 16 GB target (would restart-loop on 8 GB)
- Do NOT enable A2 71-post-upgrade.conf and B2 on new BLUE UNTIL Phase 9 verification passes (defer to Phase 10)

---

## 2. Phase 0: pre-flight (30 min)

### 2.1 Snapshot old BLUE state

```bash
ssh rope-vps 'sudo mkdir -p /root/pre-migration-2026-08-23 && cd /root/pre-migration-2026-08-23 && {
  sudo systemctl list-units --state=active --type=service --no-pager --no-legend > systemd-services.txt
  sudo systemctl list-timers --no-pager --no-legend > systemd-timers.txt
  sudo docker ps -a --format "{{.Names}} {{.Image}} {{.Status}}" > docker-containers.txt
  sudo docker volume ls --format "{{.Name}} {{.Mountpoint}}" > docker-volumes.txt
  sudo crontab -u root -l 2>/dev/null > crontab-root.txt || echo "" > crontab-root.txt
  sudo crontab -u ubuntu -l 2>/dev/null > crontab-ubuntu.txt || echo "" > crontab-ubuntu.txt
  sudo ls -1 /etc/letsencrypt/live | grep -v README > tls-certs.txt
  sudo find /opt/datachain-rope/code/deploy/nginx -name "*.conf" -exec grep -H "server_name" {} \; | grep -oE "server_name [^;]+" | sed "s/server_name //" | tr " " "\n" | sort -u | grep -v "^$" > nginx-servernames.txt
  sudo find /etc/systemd/system -name "*.service" -o -name "*.timer" | sort > systemd-unit-files.txt
  df -h > df.txt
  free -h > free.txt
  sudo du -sh /opt/datachain-rope /var/lib/docker /etc/letsencrypt /home/ubuntu/.ssh /root/.ssh /var/lib/cerber /var/lib/datachain-rope 2>&1 | tee du-sizes.txt
}'
```

### 2.2 Full service inventory (verified 2026-08-23)

Application services (17):
- `datachain-rope.service` (rope-node sealer)
- `reth-rope.service` (EVM backend)
- `dc-explorer.service` (dcscan.io backend)
- `ipfs.service` (kubo, 55 pinned CIDs)
- `cerber-mesh.service` + `cerber-mesh-alteros.service` (CERBER mesh Rope + Alteros colocated)
- `cerber-edge-ingest.service` (edge probe consumer)
- `semantic-agent.service`, `oracle-agent.service`, `insurance-agent.service`, `validation-agent.service`, `compliance-agent.service`
- `rope-idp.service` (id.datachain.network SSO)
- `rope-edc.service` (console.datachain.network)
- `rope-evm-attester.service`, `rope-evm-proposer.service`
- `rope-ecosystem-discovery.service`
- `mapstore-route.service`
- `token-publisher.service`

Docker containers (3): rope-nginx, rope-postgres, rope-redis

Timers requiring migration:
- `erpc-fleet-ha.timer` (Tier E ghost-reclaim, 30 s cadence)
- `cert-guardian.timer` (TLS validity monitor)
- `rope-shadow-witness-promote.timer` (shadow witness rotation)

Cron jobs (11):
- IPFS pinning (5x): `ipfs-pin-reth-state`, `ipfs-pin-contracts`, `ipfs-crosspin-storacha`, `ipfs-pin-loadgen-results`, `ipfs-publish-bootstrap`
- Health/watchdog (4x): `reth-health-check`, `rope-node-watchdog`, `nginx-watchdog`, `notarize-chain-checkpoints`
- Sync (1x): `edge-do-sync` (rope-cluster sync)
- Ecosystem cache-prime (1x): tanastok cache warm-up (ubuntu crontab)

### 2.3 TLS certificate inventory (7 certs)

```
agents.datachain.network
api.dcscan.io
console.datachain.network
erpc.datachain.network
erpc.rope.network
id.datachain.network
naturaproof-redirect (multi-SAN: naturaproof.io + www + probably naturaproof.net/.org)
```

Certbot renewal method to verify in Phase 4: check `/etc/letsencrypt/renewal/*.conf` for each cert's `authenticator = webroot|dns-gandi|standalone`.

### 2.4 DNS records requiring cutover (21 records, all `A` type)

Currently pointing to `92.243.26.189` (verified 2026-08-23 via nginx server_name grep):
```
agents.datachain.network
api.dcscan.io
bridge.datachain.network
compliance-agent.datachain.network
console.datachain.network
datachain.network + www.datachain.network
dcscan.io + www.dcscan.io
erpc.datachain.network
erpc.rope.network
faucet.datachain.network
id.datachain.network
naturaproof.io + www.naturaproof.io
rope.network + www.rope.network
semantic-agent.datachain.network
testnet.dcscan.io
ws.datachain.network
ws.rope.network
```

**Pre-cutover DNS action:** lower TTL on all 21 records from current (unknown, verify) to 60 s at least 2 hours before Phase 6 maintenance window. Reset TTL to 300 s or higher 24 h after cutover.

### 2.5 Cross-project peer configs pinned to 92.243.26.189

To be updated in Phase 8 after cutover:

| Project | File / config | Current pin |
|---|---|---|
| DCSwap | `/opt/dcswap/cerber-mesh/config/peers.dcswap.json` | `cerber-rope` -> `http://92.243.26.189:9107` |
| Tanastok | `/opt/tanastok/cerber-mesh/config/peers.tanastok.json` | `cerber-rope` -> `http://92.243.26.189:9107` |
| DO-rpc-1 (`157.230.18.45`) | UFW rules | `ALLOW 92.243.26.189` on 8545, 8595, 8547 |
| DO-rpc-2 (`167.172.106.174`) | UFW rules | Same |
| GREEN (`92.243.25.119`) | UFW rules | Same |
| rope-cluster-fw (DO cluster) | Firewall inbound rules | Include 92.243.26.189 |

### 2.6 lon1 vs fra1 topology note

The existing DO fleet (rope-rpc-1 at 157.230.18.45, rope-rpc-2 at 167.172.106.174, rope-offload-01 at 10.10.10.9, and 4 rope-cluster-* nodes) is in `fra1`. New BLUE in `lon1` means:

- **VPC isolation:** DO VPCs are region-scoped. New BLUE cannot join `datachain-rope-vpc` (10.10.10.0/24). All BLUE <-> DO-rpc-* communication uses public IPs + Internet.
- **Latency:** lon1 <-> fra1 = ~14 ms round-trip. Rope-node RPC calls are stateless HTTPS so this is fine. Reth p2p peering unaffected (public IP already).
- **Cross-region bandwidth:** free within DO's global egress pool for the s-8vcpu-32gb-amd tier (10 TB/mo).
- **Security posture:** UFW rules on DO-rpc-* must add new BLUE's Floating IP explicitly.

Accepted; no blocker.

---

## 3. Phase 1: provision (10 min)

### 3.1 Reserve Floating IP first

```bash
curl -sS -X POST -H "Authorization: Bearer $DO_TOKEN" -H "Content-Type: application/json" \
  -d '{"region":"lon1"}' \
  'https://api.digitalocean.com/v2/floating_ips'
```

Response includes `floating_ip.ip` - save as `NEW_BLUE_IP` in operator's secrets store.

### 3.2 Create droplet

```bash
curl -sS -X POST -H "Authorization: Bearer $DO_TOKEN" -H "Content-Type: application/json" \
  -d '{
    "name":"rope-blue-lon1",
    "region":"lon1",
    "size":"s-8vcpu-32gb-amd",
    "image":"ubuntu-24-04-x64",
    "ssh_keys":[55317676,53708118],
    "backups":true,
    "monitoring":true,
    "ipv6":false,
    "tags":["blue","sealer","production","rope"]
  }' \
  'https://api.digitalocean.com/v2/droplets'
```

Response includes `droplet.id` - save as `NEW_BLUE_ID`.

**Enable backups:** $16.80/mo (10% of droplet), weekly snapshots. Recommended given single-writer status.

### 3.3 Attach Floating IP to droplet

Wait 60-120 s for droplet to boot, then:

```bash
curl -sS -X POST -H "Authorization: Bearer $DO_TOKEN" -H "Content-Type: application/json" \
  -d "{\"type\":\"assign\",\"droplet_id\":$NEW_BLUE_ID}" \
  "https://api.digitalocean.com/v2/floating_ips/$NEW_BLUE_IP/actions"
```

Verify:
```bash
ssh -i ~/.ssh/datachain_rope_id_rsa "root@$NEW_BLUE_IP" 'curl -s https://api.ipify.org'
# Should print $NEW_BLUE_IP
```

---

## 4. Phase 2: bootstrap OS (60 min)

### 4.1 Base OS + user setup

```bash
ssh "root@$NEW_BLUE_IP" 'set -e
# Package base
apt-get update
apt-get install -y build-essential git curl rsync ufw fail2ban htop iotop \
  postgresql-client redis-tools nginx certbot python3-certbot-nginx \
  logrotate cron ca-certificates gnupg lsb-release jq

# Create ubuntu user
useradd -m -s /bin/bash -G sudo ubuntu
echo "ubuntu ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/ubuntu
chmod 440 /etc/sudoers.d/ubuntu

# Copy SSH authorized_keys from root
mkdir -p /home/ubuntu/.ssh
cp /root/.ssh/authorized_keys /home/ubuntu/.ssh/
chown -R ubuntu:ubuntu /home/ubuntu/.ssh
chmod 700 /home/ubuntu/.ssh
chmod 600 /home/ubuntu/.ssh/authorized_keys

# Install Docker
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/ubuntu/gpg | gpg --dearmor -o /etc/apt/keyrings/docker.gpg
chmod a+r /etc/apt/keyrings/docker.gpg
echo "deb [arch=$(dpkg --print-architecture) signed-by=/etc/apt/keyrings/docker.gpg] https://download.docker.com/linux/ubuntu $(. /etc/os-release && echo $VERSION_CODENAME) stable" > /etc/apt/sources.list.d/docker.list
apt-get update
apt-get install -y docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
usermod -aG docker ubuntu

# Install Rust (for future rope-node builds; not strictly needed if we ship binary only)
sudo -u ubuntu bash -c "curl --proto \"=https\" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable"

# Install kubo (matching old BLUE version 0.33.2)
cd /tmp
wget https://dist.ipfs.tech/kubo/v0.33.2/kubo_v0.33.2_linux-amd64.tar.gz
tar -xzf kubo_v0.33.2_linux-amd64.tar.gz
mv kubo/ipfs /usr/local/bin/ipfs
rm -rf kubo kubo_v0.33.2_linux-amd64.tar.gz

# Create /opt/datachain-rope + var/lib dirs
mkdir -p /opt/datachain-rope /var/lib/datachain-rope/{fleet,ledger_db} /var/log
chown -R ubuntu:ubuntu /opt/datachain-rope /var/lib/datachain-rope

# UFW: default deny, allow SSH + HTTP + HTTPS + Reth p2p + libp2p + IPFS swarm
ufw --force reset
ufw default deny incoming
ufw default allow outgoing
ufw allow 22/tcp comment "SSH"
ufw allow 80/tcp comment "HTTP (certbot renew)"
ufw allow 443/tcp comment "HTTPS (nginx)"
ufw allow 30304/tcp comment "Reth p2p"
ufw allow 30304/udp comment "Reth p2p"
ufw allow 9000/tcp comment "libp2p"
ufw allow 4001/tcp comment "IPFS swarm"
ufw allow 4001/udp comment "IPFS swarm QUIC"
# CERBER mesh peer port: restrict to peer IPs
ufw allow from 92.243.26.114 to any port 9107 proto tcp comment "cerber-dcswap"
ufw allow from 92.243.24.244 to any port 9107 proto tcp comment "cerber-tanastok"
ufw --force enable

# Deploy A2 sysctl (safe for 32 GB target)
cat > /etc/sysctl.d/99-rope-sealer.conf <<EOF
vm.swappiness = 1
vm.dirty_ratio = 5
vm.dirty_background_ratio = 2
vm.min_free_kbytes = 524288
vm.overcommit_memory = 2
vm.overcommit_ratio = 80
EOF
sysctl -p /etc/sysctl.d/99-rope-sealer.conf

# fail2ban baseline
systemctl enable --now fail2ban
'
```

### 4.2 Verify bootstrap

```bash
ssh "root@$NEW_BLUE_IP" '
echo "=== users ==="
id ubuntu
echo "=== docker ==="
docker --version
echo "=== ipfs ==="
ipfs --version
echo "=== ufw ==="
ufw status verbose
echo "=== sysctl ==="
sysctl vm.swappiness vm.dirty_ratio vm.min_free_kbytes
'
```

---

## 5. Phase 3: pre-cutover rsync (2-4 hours background)

### 5.1 SSH key path from old BLUE -> new BLUE

Install old BLUE's `ubuntu` public key on new BLUE:

```bash
BLUE_UBUNTU_PUBKEY=$(ssh rope-vps 'cat ~ubuntu/.ssh/id_ed25519.pub')
ssh "root@$NEW_BLUE_IP" "echo '$BLUE_UBUNTU_PUBKEY' >> /home/ubuntu/.ssh/authorized_keys && chmod 600 /home/ubuntu/.ssh/authorized_keys"
```

### 5.2 Hot rsync (source active, target quiet)

Do NOT stop any service on old BLUE during Phase 3. Rsync tolerates active source; we do a final delta rsync in Phase 6.

```bash
# From your local workstation, orchestrate the rsync via old BLUE (has network path to new BLUE)
ssh rope-vps "sudo -u ubuntu bash -c 'set -e

# Rsync /opt/datachain-rope (source, config, scripts)
rsync -avz --delete --exclude \".git\" --exclude \"target/\" --exclude \"node_modules/\" \
  /opt/datachain-rope/ ubuntu@$NEW_BLUE_IP:/opt/datachain-rope/

# Rsync IPFS repo (7.8 GB)
rsync -avz --delete /opt/datachain-rope/ipfs/ ubuntu@$NEW_BLUE_IP:/opt/datachain-rope/ipfs/

# Rsync Reth data dir (11 GB) - excluding hot mempool/pipeline files
rsync -avz --exclude \"static_files.\\*\" /opt/datachain-rope/reth/data/ ubuntu@$NEW_BLUE_IP:/opt/datachain-rope/reth/data/

# Rsync rope-node ledger RocksDB
rsync -avz /var/lib/datachain-rope/ledger_db/ ubuntu@$NEW_BLUE_IP:/var/lib/datachain-rope/ledger_db/
rsync -avz /var/lib/datachain-rope/fleet/ ubuntu@$NEW_BLUE_IP:/var/lib/datachain-rope/fleet/

# Rsync CERBER mesh state
sudo rsync -avz /var/lib/cerber/ root@$NEW_BLUE_IP:/var/lib/cerber/

# Rsync /etc systemd + certbot + cron + nginx (via root)
sudo rsync -avz /etc/systemd/system/ root@$NEW_BLUE_IP:/etc/systemd/system/
sudo rsync -avz /etc/letsencrypt/ root@$NEW_BLUE_IP:/etc/letsencrypt/
sudo rsync -avz /var/spool/cron/crontabs/ root@$NEW_BLUE_IP:/var/spool/cron/crontabs/
'"
```

**Estimated rsync duration:** 2-4 hours over Gandi -> DO Internet path (rough ~10-20 Mbit/s sustained, 19 GB total ≈ 3-4 hours). Bandwidth is a bottleneck, not disk.

### 5.3 Docker container migration

For Postgres and Redis, we need consistent volume snapshots. Postgres:

```bash
# On old BLUE - hot pg_dump (does NOT lock)
ssh rope-vps 'sudo docker exec rope-postgres pg_dumpall -U postgres | gzip > /tmp/rope-postgres-dump.sql.gz'

# Copy dump to new BLUE
ssh rope-vps "scp /tmp/rope-postgres-dump.sql.gz ubuntu@$NEW_BLUE_IP:/tmp/"

# On new BLUE - start postgres container, then restore
ssh "ubuntu@$NEW_BLUE_IP" '
docker run -d --name rope-postgres \
  -e POSTGRES_PASSWORD=$(cat /opt/datachain-rope/.env.postgres | grep POSTGRES_PASSWORD | cut -d= -f2) \
  -v rope-postgres-data:/var/lib/postgresql/data \
  postgres:16-alpine
sleep 10
zcat /tmp/rope-postgres-dump.sql.gz | docker exec -i rope-postgres psql -U postgres
docker stop rope-postgres  # will be started fresh in Phase 6
'
```

Redis is ephemeral in this deployment; no state migration needed. Start fresh on new BLUE.

### 5.4 rope-nginx container image + config

nginx runs as `nginx:alpine` container. Config lives in `/opt/datachain-rope/code/deploy/nginx/`. Rsynced above. Nginx will start fresh in Phase 6.

---

## 6. Phase 4: TLS certificate preparation (30 min)

Choose based on operator answer to D1.

### 6.1 Option A: rsync certs and force renewal via HTTP-01 pre-cutover

Certs are already rsynced in Phase 5.2. Nginx on new BLUE cannot serve `.well-known/acme-challenge` because DNS still points to old BLUE. So HTTP-01 renewal won't work on new BLUE until after DNS cutover.

**Workaround:** Old BLUE `certbot renew` runs from a cron; certs are valid for 90 days. If cutover happens within cert validity, we can leave renewal to the new BLUE post-cutover. Add:

```bash
ssh "root@$NEW_BLUE_IP" '
systemctl enable certbot.timer  # inherited from rsync but confirm enabled
'
```

Post-cutover, first renewal (via cron in ~1-30 days) validates HTTP-01. If it fails, use `--dns-gandi` per Option B as recovery.

### 6.2 Option B: DNS-01 challenge pre-cutover (recommended if Gandi API creds exist)

Check for existing Gandi DNS API creds:
```bash
ssh rope-vps 'sudo ls /etc/letsencrypt/ | grep -i gandi'
ssh rope-vps 'sudo find /etc/letsencrypt/renewal -name "*.conf" -exec grep -l "dns-gandi\|dns_gandi" {} \;'
```

If found, install certbot-dns-gandi on new BLUE and re-issue via DNS-01:
```bash
ssh "root@$NEW_BLUE_IP" '
apt-get install -y python3-certbot-dns-gandi
for domain in agents.datachain.network api.dcscan.io console.datachain.network erpc.datachain.network erpc.rope.network id.datachain.network; do
  certbot certonly --dns-gandi --dns-gandi-credentials /etc/letsencrypt/gandi-creds.ini -d "$domain" --non-interactive --agree-tos -m contact@datachain.one
done
# naturaproof-redirect (multi-SAN)
certbot certonly --dns-gandi --dns-gandi-credentials /etc/letsencrypt/gandi-creds.ini \
  -d naturaproof.io -d www.naturaproof.io -d naturaproof.net -d www.naturaproof.net -d naturaproof.org -d www.naturaproof.org \
  --cert-name naturaproof-redirect --non-interactive --agree-tos -m contact@datachain.one
'
```

If Gandi API creds do NOT exist, ask operator whether to create them (Gandi web console -> Account -> API Keys) or fall back to Option A.

### 6.3 Verify certs valid on new BLUE

```bash
ssh "root@$NEW_BLUE_IP" 'certbot certificates 2>&1 | grep -E "Certificate Name|Expiry Date"'
```

---

## 7. Phase 5: peer pre-registration (30 min)

### 7.1 Add new BLUE Floating IP to firewalls (before cutover, so mesh probes work in both directions)

DO-rpc-1 (`157.230.18.45`):
```bash
ssh -i ~/.ssh/datachain_rope_id_rsa "root@157.230.18.45" "
ufw insert 1 allow from $NEW_BLUE_IP to any port 8545,8595,8547 proto tcp comment 'rope-blue-lon1'
"
```

DO-rpc-2 (`167.172.106.174`): same.

GREEN (`92.243.25.119` on Gandi):
```bash
ssh anvil-vps "sudo ufw insert 1 allow from $NEW_BLUE_IP to any port 8545,8595,8547 proto tcp comment 'rope-blue-lon1'"
```

rope-cluster-* nodes: UFW rules similar.

### 7.2 Announce new BLUE peer on DCSwap and Tanastok mesh peer configs (do NOT remove old BLUE yet)

Send handover to DCSwap and Tanastok agents:

- DCSwap: add `cerber-rope-lon1` to `/opt/dcswap/cerber-mesh/config/peers.dcswap.json` with same pubkey (`7acee395152891bcf45aee369a72d24dfb5259f056736e157e012f13416b787f`) but new URL `http://$NEW_BLUE_IP:9107`. Keep `cerber-rope` (old BLUE) for now.
- Tanastok: same pattern for `peers.tanastok.json`.

This way both old and new BLUE are recognized during cutover. Cleanup in Phase 8.

### 7.3 Lower DNS TTLs 2 hours before Phase 6

Via Gandi DNS control panel (or API if creds available), lower all 21 A records to TTL=60. Confirm propagation:
```bash
dig +short @8.8.8.8 erpc.datachain.network
# should return 92.243.26.189 (unchanged content, lower TTL)
```

---

## 8. Phase 6: MAINTENANCE WINDOW - writes blocked (60-120 min)

### 8.1 Announce window to ecosystem

Send handover 24 h in advance to DCSwap, Tanastok, Datawallet+, and Alteros agents. Announce writes-blocked window on `dcswap.net` banner + `dcscan.io` status page.

### 8.2 Suppress CERBER pages during window

- DCSwap R12/R13: set `CERBER_R12_SUSTAIN_SECS` to 3600 or disable rule for the window
- Tanastok R12: same
- Alteros CERBER (colocated): stops naturally when we stop mesh service

### 8.3 On old BLUE: stop write services (writer + attester + agents that emit knots)

```bash
ssh rope-vps 'sudo bash -c "
# Stop writer + attester (blocks eth_sendRawTransaction + destructive rope_*)
systemctl stop datachain-rope.service
systemctl stop reth-rope.service
# Stop knot-emitter agents (they will backfill from Phase 8)
systemctl stop rope-evm-attester.service rope-evm-proposer.service
systemctl stop semantic-agent.service oracle-agent.service insurance-agent.service validation-agent.service compliance-agent.service
systemctl stop token-publisher.service
systemctl stop rope-ecosystem-discovery.service
# Stop mesh so DCSwap R13 sees offline
systemctl stop cerber-mesh.service cerber-mesh-alteros.service cerber-edge-ingest.service
# Stop supply/dc-explorer (dcscan front)
systemctl stop dc-explorer.service
# Stop identity/console services
systemctl stop rope-idp.service rope-edc.service
# Stop mapstore
systemctl stop mapstore-route.service
# Stop cron (prevent IPFS pin churn)
systemctl stop cron
# ipfs.service stays up for now - stop last after final rsync
"'
```

### 8.4 Final delta rsync

```bash
# From workstation
ssh rope-vps "sudo -u ubuntu bash -c '
rsync -avz --delete /opt/datachain-rope/ ubuntu@$NEW_BLUE_IP:/opt/datachain-rope/
rsync -avz --delete /opt/datachain-rope/ipfs/ ubuntu@$NEW_BLUE_IP:/opt/datachain-rope/ipfs/
rsync -avz /opt/datachain-rope/reth/data/ ubuntu@$NEW_BLUE_IP:/opt/datachain-rope/reth/data/
rsync -avz /var/lib/datachain-rope/ledger_db/ ubuntu@$NEW_BLUE_IP:/var/lib/datachain-rope/ledger_db/
rsync -avz /var/lib/datachain-rope/fleet/ ubuntu@$NEW_BLUE_IP:/var/lib/datachain-rope/fleet/
sudo rsync -avz /var/lib/cerber/ root@$NEW_BLUE_IP:/var/lib/cerber/
sudo rsync -avz /etc/systemd/system/ root@$NEW_BLUE_IP:/etc/systemd/system/
sudo rsync -avz /etc/letsencrypt/ root@$NEW_BLUE_IP:/etc/letsencrypt/
sudo rsync -avz /var/spool/cron/crontabs/ root@$NEW_BLUE_IP:/var/spool/cron/crontabs/
'"

# Redo Postgres dump (recent state)
ssh rope-vps 'sudo docker exec rope-postgres pg_dumpall -U postgres | gzip > /tmp/rope-postgres-dump-final.sql.gz'
ssh rope-vps "scp /tmp/rope-postgres-dump-final.sql.gz ubuntu@$NEW_BLUE_IP:/tmp/"
```

### 8.5 Stop IPFS on old BLUE (final)

```bash
ssh rope-vps 'sudo systemctl stop ipfs.service'
# Final IPFS delta
ssh rope-vps "sudo -u ubuntu rsync -avz --delete /opt/datachain-rope/ipfs/ ubuntu@$NEW_BLUE_IP:/opt/datachain-rope/ipfs/"
```

### 8.6 Start services on new BLUE

```bash
ssh "root@$NEW_BLUE_IP" '
systemctl daemon-reload
# Docker containers first
docker start rope-postgres || docker run -d --name rope-postgres \
  -e POSTGRES_PASSWORD=$(cat /opt/datachain-rope/.env.postgres | grep POSTGRES_PASSWORD | cut -d= -f2) \
  -v rope-postgres-data:/var/lib/postgresql/data \
  postgres:16-alpine
zcat /tmp/rope-postgres-dump-final.sql.gz | docker exec -i rope-postgres psql -U postgres
docker run -d --name rope-redis redis:7-alpine
docker run -d --name rope-nginx -v /opt/datachain-rope/code/deploy/nginx:/etc/nginx/conf.d:ro -v /etc/letsencrypt:/etc/letsencrypt:ro -p 80:80 -p 443:443 nginx:alpine

# Services in dependency order
systemctl start ipfs.service
sleep 5
systemctl start reth-rope.service
sleep 15  # wait for reth p2p sync + rpc up
systemctl start datachain-rope.service
sleep 10  # wait for rope-node ledger open
systemctl start dc-explorer.service
systemctl start rope-idp.service rope-edc.service
systemctl start rope-evm-attester.service rope-evm-proposer.service
systemctl start semantic-agent.service oracle-agent.service insurance-agent.service validation-agent.service compliance-agent.service
systemctl start token-publisher.service rope-ecosystem-discovery.service mapstore-route.service
systemctl start cerber-mesh.service cerber-mesh-alteros.service cerber-edge-ingest.service
systemctl start cron

# Timers
systemctl enable --now erpc-fleet-ha.timer cert-guardian.timer rope-shadow-witness-promote.timer
'
```

### 8.7 Local verification on new BLUE (before DNS cutover)

```bash
ssh "root@$NEW_BLUE_IP" '
curl -sS -X POST http://127.0.0.1:8545 -H "content-type: application/json" \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_blockNumber\",\"params\":[]}"
# expect a block number matching old BLUE last-known + delta
'
```

If PASS -> proceed to Phase 7. If FAIL -> Phase 6 rollback (restart services on old BLUE).

---

## 9. Phase 7: DNS cutover (5-30 min)

### 9.1 Batch cutover via Gandi DNS API

Update all 21 A records from `92.243.26.189` to `$NEW_BLUE_IP` in one API push (Gandi supports LiveDNS bulk update). Keep TTL=60.

Verify from multiple public resolvers:
```bash
for r in 8.8.8.8 1.1.1.1 9.9.9.9; do
  echo "=== resolver $r ==="
  for d in erpc.datachain.network dcscan.io id.datachain.network agents.datachain.network; do
    echo "$d: $(dig +short @$r $d)"
  done
done
```

### 9.2 Public verification once DNS propagates

```bash
# HTTPS certificate valid on new IP
curl -sSI https://erpc.datachain.network/v1/fleet-status | grep -i "server\|content-type"
curl -sS https://erpc.datachain.network/v1/fleet-status | jq '.writer.status, .edge.status'
curl -sSI https://dcscan.io/api/v1/stats | head
curl -sSI https://id.datachain.network/healthz
```

---

## 10. Phase 8: cross-project peer swap (30 min)

Once new BLUE is confirmed live via DNS, hand over peer configs from old BLUE IP to new BLUE Floating IP.

### 10.1 Handover to DCSwap agent

```markdown
# HANDOVER - BLUE migrated to lon1

BLUE writer now at $NEW_BLUE_IP (was 92.243.26.189).

Please update /opt/dcswap/cerber-mesh/config/peers.dcswap.json:
- Remove cerber-rope entry pointing to 92.243.26.189
- Rename cerber-rope-lon1 (pre-registered in Phase 5) to cerber-rope
- Pubkey unchanged: 7acee395152891bcf45aee369a72d24dfb5259f056736e157e012f13416b787f
```

### 10.2 Handover to Tanastok agent

Same pattern for `/opt/tanastok/cerber-mesh/config/peers.tanastok.json`.

### 10.3 UFW cleanup on DO-rpc-*, GREEN

Remove old `92.243.26.189` allow rules after 24 h stability window (in Phase 10).

---

## 11. Phase 9: post-cutover verify (60 min)

### 11.1 Acceptance checklist

- [ ] `curl https://erpc.datachain.network/v1/fleet-status` -> writer.status=healthy, edge.status=healthy
- [ ] `curl https://erpc.datachain.network -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'` -> block advancing
- [ ] `curl https://erpc.datachain.network/v1/read -d '{"jsonrpc":"2.0","id":1,"method":"eth_sendRawTransaction","params":["0x00"]}'` -> HTTP 405
- [ ] `curl https://dcscan.io/api/v1/stats` -> live totals
- [ ] `curl https://dcscan.io/api/v1/supply/circulating` -> Scenario A ~3.73B
- [ ] `curl https://id.datachain.network/healthz` -> 200
- [ ] `curl https://agents.datachain.network/v1/gdpr/article17` -> 200
- [ ] `curl https://erpc.datachain.network/v1/cerber/mesh-status` -> all peers reachable
- [ ] `ipfs id` on new BLUE -> same PeerID as old BLUE
- [ ] `ipfs pin ls --type recursive | wc -l` -> 55 (or higher)
- [ ] DCSwap CERBER R13 verdict ok, coverage 100
- [ ] Tanastok mesh coverage 100
- [ ] Old BLUE responds only to SSH; all services stopped

### 11.2 24-hour observation

- MTBF on new BLUE > 24 h -> proceed to Phase 10 (schedule Gandi decommission)
- MTBF < 24 h -> investigate; consider rollback to old BLUE via DNS reversion

---

## 12. Phase 10: fallback observation (7-14 days) + A2/B2 enablement

### 12.1 Enable A2 71-post-upgrade.conf and B2 memory circuit breaker on new BLUE

Now that 32 GB RAM confirmed:

```bash
scp datachain-rope/deploy/systemd/datachain-rope.service.d/71-memory-swap-post-upgrade.conf "root@$NEW_BLUE_IP:/etc/systemd/system/datachain-rope.service.d/"
scp datachain-rope/deploy/systemd/datachain-rope.service.d/72-memory-circuit-breaker.conf "root@$NEW_BLUE_IP:/etc/systemd/system/datachain-rope.service.d/"
ssh "root@$NEW_BLUE_IP" 'systemctl daemon-reload && systemctl restart datachain-rope.service'
```

Verify:
```bash
ssh "root@$NEW_BLUE_IP" 'cat /sys/fs/cgroup/system.slice/datachain-rope.service/memory.swap.max'
# expect: 0
ssh "root@$NEW_BLUE_IP" 'cat /var/lib/datachain-rope/self-watchdog.json | jq ".memory_circuit_breach_since, .memory_circuit_trips"'
# expect: null, 0
```

### 12.2 Old BLUE stays alive as read-only fallback

Old BLUE stopped services; reth + rope-node in stopped state. DNS points to new BLUE. If new BLUE catastrophically fails, we can:
1. Point DNS back to 92.243.26.189
2. Start reth-rope + datachain-rope on old BLUE
3. Ledger will resync from network (assume some knot gap accepted)

Do NOT let old BLUE start writing while new BLUE is writing (dual-writer ledger fork). Guard via nginx `rpc_primary_only` upstream still pointing at new BLUE.

---

## 13. Phase 11: old BLUE decommission (15 min, D3-gated)

After D3-week observation with new BLUE MTBF > 7 days:

1. Confirm no unexpected traffic on old BLUE (`sudo tail /var/log/nginx/access.log`).
2. Notify Gandi to terminate the VPS via web console.
3. Reduce DNS TTL back to 300 s or higher.
4. Remove UFW rules on DO-rpc-*, GREEN allowing old 92.243.26.189.
5. If D2 = Option X: destroy rope-offload-01 too.

---

## 14. Rollback plan (each phase)

| Phase | Rollback trigger | Rollback action |
|---|---|---|
| Phase 1-2 (bootstrap) | Droplet creation fails | Retry; nothing on old BLUE changed |
| Phase 3 (rsync) | Rsync fails or new BLUE runs out of disk | Fix on new BLUE; retry; old BLUE untouched |
| Phase 4 (TLS) | Certbot fails | Fall back to Option A (rsync certs); if that fails, delay Phase 6 |
| Phase 5 (peer pre-reg) | UFW change breaks a fleet node | Remove UFW rule; old BLUE unaffected |
| Phase 6 (maintenance) | New BLUE services fail to start | Restart services on old BLUE; skip DNS cutover; investigate |
| Phase 7 (DNS) | Public HTTPS breaks | Revert DNS to 92.243.26.189; restart services on old BLUE |
| Phase 8 (peer swap) | CERBER mesh breaks | Restore both peer entries; old BLUE mesh still cached |
| Phase 9 (verify) | MTBF regression | Revert DNS; keep new BLUE as read-only follower for study |
| Phase 10 (observation) | Any anomaly | Revert DNS + restart old BLUE writer services |
| Phase 11 (decommission) | Post-decommission bug | Cannot recover Gandi BLUE. Must rely on new BLUE + backups |

---

## 15. Estimated timeline (calendar)

| Day | Activity |
|---|---|
| Day 0 (approval + Phase 0) | Operator confirms D1/D2/D3; ROPE agent runs pre-flight snapshot |
| Day 1 (Phase 1-4) | Provision + bootstrap + first rsync + TLS prep |
| Day 2 (Phase 5) | Peer pre-registration + DNS TTL lowering |
| Day 3 (Phase 6-9) | Maintenance window (60-120 min) + verify (60 min) |
| Days 4-10 (Phase 10) | Observation + A2 71-post + B2 enablement |
| Day 11-14 (Phase 11) | Decommission Gandi BLUE |

**Operator time:** ~4-6 hours of active work spread over 4 days, plus 60-120 min supervised maintenance window on Day 3.

---

## 16. Cross-references

- `docs/A3_ALTERNATIVES_GANDI_QUOTA_BLOCK_2026-08-23.md` - option selection background
- `docs/ROPE_VPS_MEMORY_UPGRADE_RUNBOOK_2026-08-23.md` - superseded by this runbook
- `docs/MTBF_REGRESSION_POSTMORTEM_AND_MITIGATION_MENU_2026-08-23.md` - why we are doing this
- `docs/P0_P1_P2_INTEGRATED_SEQUENCE_2026-08-23.md` - A2 pre-upgrade already deployed; 71-post-upgrade.conf and B2 pending Phase 10
- `.cursor/rules/handover-canonical-agents-live-from-rope-2026-05-05.mdc` - the 5 canonical agents that must stay running through the migration
- `.cursor/rules/handover-to-tanastok-cerber-mesh-peer-live-2026-08-03.mdc` - CERBER mesh peer contract
- `.cursor/rules/handover-datachain-id-sso-live-2026-07-07.mdc` - id.datachain.network SSO service on new BLUE
- `.cursor/rules/datachain-rope-production-roadmap.mdc` - operational SSH conventions
