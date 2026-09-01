# CERBER mesh — ecosystem signed verification

Production package shared conceptually across **Rope**, **DCSwap**, **Tanastok**, and **Alteros**.

## Guarantees

1. **Nodes sign interactions** — Ed25519 (`ed25519-cerber-mesh-v1`), domain-separated (`DCROPE/cerber-mesh/v1`).
2. **CERBER verifies Rope interactions** — every tick audits fleet-status (+ signature), ghost_reclaim observation, and a fixed RPC set (`eth_chainId`, `eth_blockNumber`, `rope_globalStats`, `web3_clientVersion`). Results go to append-only NDJSON under `/var/lib/datachain-rope/cerber/audit/`.
3. **Detailed report** — signed `detailed-report` with coverage %, per-kind breakdown, merkle root of verified bodies, mesh peer reachability.
4. **Mesh** — peers heartbeat + ingest signed reports over HTTP. DCSwap CERBER **R13** pages on signature failure or coverage &lt; 100%.

## Quick start (Rope)

```bash
sudo mkdir -p /opt/datachain-rope/cerber /var/lib/datachain-rope/cerber
sudo rsync -a deploy/cerber/ /opt/datachain-rope/cerber/
sudo cp deploy/systemd/cerber-mesh.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now cerber-mesh.service
```

Public:

```bash
curl -sS https://erpc.datachain.network/v1/fleet-status.sig.json | jq .
curl -sS https://erpc.datachain.network/v1/cerber/report | jq '.body | {coverage_pct, all_verified, total}'
curl -sS https://erpc.datachain.network/v1/cerber/mesh-status | jq .
```

## Alteros / Tanastok / DCSwap

Install the same tree, set:

| Env | Example |
|---|---|
| `CERBER_PEER_ID` | `cerber-alteros` / `cerber-tanastok` / `cerber-dcswap` |
| `CERBER_PEER_ROLE` | `alteros` / `tanastok` / `dcswap` |
| `CERBER_MESH_PORT` | `9107` (Alteros may use `9108` if co-located) |
| `CERBER_PEER_ALTEROS_URL` | `http://<alteros-host>:9107` |

DCSwap additionally runs **R13** via `cerber-sentinel.mjs --rule R13`.

### Tanastok (live since 2026-08-03)

Tanastok host CERBER (`/opt/cerber` Python orchestrator) is a **different** product from the ecosystem mesh peer. The mesh peer is:

| Item | Value |
|---|---|
| Package | `/opt/tanastok/cerber-mesh/` |
| Unit | `tanastok-cerber-mesh.service` |
| Peers overlay | `config/peers.tanastok.json` |
| Identity | `/var/lib/cerber/tanastok-mesh-identity.pem` |
| Listen | `0.0.0.0:9107` (UFW allow from rope-vps `92.243.26.189` + dcswap-prod `92.243.26.114`) |
| Peer id / kid | `cerber-tanastok` / `ba451eb4d2f5aeb6` |

Unit source: `deploy/systemd/tanastok-cerber-mesh.service`.

## Tests

```bash
cd deploy/cerber && node --test test/*.test.mjs
```
