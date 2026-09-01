# Fleet resilience P0/P1 (2026-09-01)

**Status:** Shipped in source; deploy to London + followers per commands below.

## Problem (Paris 09:10Z restart)

London cron `reth-snapshot-replicate.sh` stopped Paris `reth-rope` + `datachain-rope` every ~10 min while Paris remained in nginx read pools -> public 502 bursts and attester HA `rpc_probe_fail`.

Root cause was **not** OOM or attester HA. Reference lag threshold (100 blocks) also forced full resync when Paris was only ~144 blocks behind.

## P0 fixes

| Item | Mechanism |
|---|---|
| Read-pool drain | `read-pool-drain-follower.sh` + nginx `includes/read-pool/*.inc` - drain before rsync, undrain after :8545 healthy |
| Start order | `reth-rope` -> poll :8595 up to 120s -> `rope-evm-attester` -> `datachain-rope` |
| Rsync hygiene | `--exclude='*.tmp'` on static_files; verify `mdbx.dat` non-empty before start |
| Reference lag | `REFERENCE_LAG_BLOCKS=512` (was implicit 100 via PRIMARY-100) |

## P1 fixes

| Item | Mechanism |
|---|---|
| Live catch-up | `CATCH_UP_RPC_URL=http://159.65.208.206:8595` on all attesters |
| UFW London | `ufw-writer-catch-up-peers.sh` allows :8595 from GREEN/Paris/DO |
| Less forced resync | 512-block hash check lets attesters catch up between replicate ticks |

## P2

| Item | Mechanism |
|---|---|
| DNS failover | `erpc-dns-failover.timer` on DO-rpc-1 (needs `/etc/datachain-rope/dns-failover.env` with `GANDI_API_KEY`) |
| Edge ingest | `cerber-edge-ingest` ExecStartPre touches `external-probes.ndjson` so fleet-status shows `enabled:true` |

## Deploy (London new-blue)

```bash
ROPE=/Users/kazealphonseonguene/Downloads/DATACHAIN\ ROPE/datachain-rope
LON=root@159.65.208.206
KEY=~/.ssh/datachain_rope_id_rsa

rsync -avz -e "ssh -i $KEY" \
  "$ROPE/deploy/scripts/read-pool-drain-follower.sh" \
  "$ROPE/deploy/scripts/reth-snapshot-replicate.sh" \
  "$ROPE/deploy/scripts/ufw-writer-catch-up-peers.sh" \
  $LON:/opt/datachain-rope/scripts/

rsync -avz -e "ssh -i $KEY" \
  "$ROPE/deploy/nginx/conf.d/datachain.network.conf" \
  $LON:/opt/datachain-rope/code/deploy/nginx/conf.d/

rsync -avz -e "ssh -i $KEY" \
  "$ROPE/deploy/nginx/conf.d/includes/read-pool/" \
  $LON:/opt/datachain-rope/code/deploy/nginx/conf.d/includes/read-pool/

ssh -i $KEY $LON 'chmod +x /opt/datachain-rope/scripts/read-pool-drain-follower.sh /opt/datachain-rope/scripts/reth-snapshot-replicate.sh /opt/datachain-rope/scripts/ufw-writer-catch-up-peers.sh && \
  bash /opt/datachain-rope/scripts/ufw-writer-catch-up-peers.sh && \
  docker exec rope-nginx nginx -t && docker exec rope-nginx nginx -s reload'
```

## Deploy attester catch-up (each follower)

```bash
# GREEN, Paris, DO-1, DO-2 - copy 10-catch-up-writer.conf, daemon-reload, restart attester
```

## Verify next replicate cycle (:07,:17,...)

```bash
ssh -i $KEY $LON 'tail -30 /home/ubuntu/log/reth-snapshot-replicate.log'
ssh -i $KEY $LON '/opt/datachain-rope/scripts/read-pool-drain-follower.sh status ParisLegacy'
curl -sS https://erpc.datachain.network/v1/fleet-status | jq '.edge.external_probes.enabled'
```

## Rollback

- Restore previous `datachain.network.conf` from `.pre-promote-*` or git
- `undrain` all followers: `read-pool-drain-follower.sh undrain ParisLegacy` (etc.)
- Revert `REFERENCE_LAG_BLOCKS` to 100 only if operator wants old behavior (not recommended)
