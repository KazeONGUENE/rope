# Datachain Rope — Canonical AI Agents Deployment

This directory contains everything needed to deploy the five canonical
AI testimony agents to a Datachain Rope VPS:

| Agent | Binary | Listens? | Purpose |
|---|---|---|---|
| OracleAgent | `oracle-agent` | no (outbound only) | Pulls DC FAT price from `dcswap.net/v1/prices` and anchors a signed `OraclePriceTestimony` knot every 60s. |
| ValidationAgent | `validation-agent` | no (outbound only) | Polls new cord anchor knots, verifies post-quantum signatures, anchors a signed `ValidationTestimony` for each valid one. |
| InsuranceAgent | `insurance-agent` | no (outbound only) | Refreshes Tanastok tokenized RWAs hourly, computes parametric risk profiles, anchors signed `ParametricInsuranceAttestation` knots. |
| SemanticAgent | `semantic-agent` | yes — `:9092/v1/search` | Indexes new knots, exposes HTTP semantic search, anchors a merkle-rooted `IndexCheckpointTestimony` every 10 min. |
| ComplianceAgent | `compliance-agent` | yes — `:9091/v1/gdpr` | Receives GDPR Art. 17 erasure requests, orchestrates `rope_untieKnot`, anchors `ComplianceTestimony` knots covering MiFID II / DORA. |

Each agent owns a canonical wallet (`0x…C001` through `0x…C005`) which
is the on-chain identity used for its signed testimonies. These wallets
are visible on DCScan: <https://dcscan.io/agents>.

## Layout

```
deploy/agents/
├── README.md                       this file
├── install-agent.sh                build + install + register a single agent
├── systemd/
│   ├── oracle-agent.service
│   ├── validation-agent.service
│   ├── insurance-agent.service
│   ├── semantic-agent.service
│   └── compliance-agent.service
├── env/
│   ├── shared.env.example          chain-wide settings (RPC URL, log level)
│   ├── oracle-agent.env.example
│   ├── validation-agent.env.example
│   ├── insurance-agent.env.example
│   ├── semantic-agent.env.example
│   └── compliance-agent.env.example
└── nginx/
    └── agents.datachain.network.conf  TLS-terminated reverse proxy for the
                                       two HTTP-exposing agents
```

## Prerequisites on the VPS

- Ubuntu 22.04+ with the existing Datachain Rope deploy under
  `/home/ubuntu/datachain-rope` (i.e. the same host running
  `datachain-rope.service` and `dc-explorer.service`).
- Rust toolchain available for the `ubuntu` user (`/home/ubuntu/.cargo/bin`).
- `rope-node` reachable at `http://127.0.0.1:8545` (the agents talk to
  the local node only — they never connect to the public RPC).
- nginx 1.18+ with TLS already terminated for `*.datachain.network`
  (the agents reuse the same Let's Encrypt cert via SNI).

## Quick install (all five agents)

From the repo root **on the VPS** after the agent crates have merged
into `main`:

```bash
cd /home/ubuntu/datachain-rope
git pull origin main

# Build all five at once (release profile)
cargo build --release \
    -p oracle-agent \
    -p validation-agent \
    -p insurance-agent \
    -p semantic-agent \
    -p compliance-agent

# Install + enable each one
for agent in oracle validation insurance semantic compliance; do
    sudo deploy/agents/install-agent.sh "${agent}-agent"
done

# Confirm
systemctl status oracle-agent validation-agent insurance-agent \
                 semantic-agent compliance-agent
```

The installer script:

1. Creates `/etc/<agent-name>/` with mode `0700` owned by `ubuntu:ubuntu`.
2. Creates `/var/lib/<agent-name>/` for state (semantic-agent index,
   compliance-agent dedup cache, etc.).
3. Drops the per-agent `.env` template at `/etc/<agent-name>/config.env`
   if no file exists (does **not** overwrite an existing one).
4. Drops the shared template at `/etc/datachain-agents/shared.env` if
   absent.
5. For agents that need a key (oracle, validation, compliance,
   insurance), runs `<agent>-agent init-key --path /etc/<agent-name>/<agent>.seed`
   if the seed file is absent. Mode `0600`, `ubuntu:ubuntu`.
6. Installs the systemd unit to `/etc/systemd/system/`, runs
   `daemon-reload`, then `enable --now`.

Re-running the script for an already-installed agent is safe: it
upgrades the systemd unit and restarts the service without touching
the env file or the key.

## Per-agent CLI assumptions

Until each crate's `--help` is verified against its merged `main.rs`,
the systemd units assume the CLI shape from each subagent's
specification. If a flag name differs, edit the unit's `ExecStart` (or
preferably move the value into the per-agent `.env` so the unit stays
stable). The known shapes are:

- **oracle-agent**: `oracle-agent [--feed-url] [--rpc-url] [--wallet-hex]
  [--interval-secs] [--key-path] [--signing-mode {hybrid,ed25519-only}]`
  — also reads `ORACLE_*` env vars.
- **insurance-agent**: `insurance-agent serve [--rpc-url] [--tanastok-url]
  [--interval-secs] [--reattest-after-secs] [--agent-wallet]`
  — also reads `INSURANCE_*` env vars.
- **validation-agent**: `validation-agent [--rpc-url] [--poll-interval-secs]
  [--key-path] [--anchor-only]` — assumed, verify on merge.
- **semantic-agent**: `semantic-agent serve [--listen] [--rpc-url]
  [--index-path] [--checkpoint-interval-secs]` — assumed, verify on merge.
- **compliance-agent**: `compliance-agent serve [--listen] [--rpc-url]
  [--key-path]` — assumed, verify on merge.

## Verifying an agent is anchoring

Each agent anchors testimony knots from its canonical wallet. After
starting, check the on-chain testimony stream:

```bash
# Tail journal
sudo journalctl -u oracle-agent -f

# Check the wallet's recent activity on DCScan
xdg-open https://dcscan.io/address/0x000000000000000000000000000000000000C002

# Or via the local API
curl -s http://127.0.0.1:8545 \
    -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","method":"rope_walkLedgerChain","params":[{"wallet":"0x000000000000000000000000000000000000C002","direction":"backward","limit":5}],"id":1}'
```

DCScan's `/api/v1/ai-agents` automatically picks up `last_anchor_at`,
`testimoniesCount`, and per-wallet uptime once the on-chain history
exists; no separate registration step is needed.

## nginx — public endpoints for HTTP agents

`nginx/agents.datachain.network.conf` reverse-proxies the two agents
that expose HTTP:

- `https://semantic-agent.datachain.network/` → `127.0.0.1:9092`
- `https://compliance-agent.datachain.network/` → `127.0.0.1:9091`

The other three agents (oracle, validation, insurance) have no HTTP
listener — they only push outbound RPC to the local rope-node. Their
metadata is exposed read-only via `dcscan.io/agents`. If you later want
to expose them too (e.g. an `oracle-agent /v1/prices` mirror), add a
small axum module in the respective crate and extend the nginx config.

## Operational tips

- Pin a single agent's verbosity at runtime:
  `sudo systemctl set-environment ORACLE_LOG=trace,oracle_agent=trace`
  then `sudo systemctl restart oracle-agent`.
- Rotate an agent's keypair: stop the service, move the old seed file
  aside, run `<agent>-agent init-key --path /etc/<agent>/<agent>.seed`,
  start the service. The on-chain wallet stays the same (the wallet is
  `0x…C00x`, not the keypair address); only the signing key changes.
- All five units include `RestartSec=10` and `Restart=on-failure`.
  systemd will not loop infinitely — `StartLimitIntervalSec=600` and
  `StartLimitBurst=10` cap restarts to 10 per 10 minutes.
- The agents are independent processes with no shared state; you can
  run any subset.

## Tearing down

```bash
for agent in oracle validation insurance semantic compliance; do
    sudo systemctl disable --now "${agent}-agent"
    sudo rm -f "/etc/systemd/system/${agent}-agent.service"
done
sudo systemctl daemon-reload

# Optional — wipes keys, env files, and state. The on-chain
# testimony history stays intact; only the local agent identity is lost.
sudo rm -rf /etc/{oracle,validation,insurance,semantic,compliance}-agent \
            /var/lib/{oracle,validation,insurance,semantic,compliance}-agent \
            /etc/datachain-agents
```
