# Datachain Rope — Node Deployment Package

Run your own Datachain Rope node — on a VPS/VM, or on a
[Databox](https://databox.network) device — in one command.

This package does **not** join the fixed 4-node EVM-quorum committee that
proposes/attests blocks (BLUE / GREEN / DO-rpc-1 / DO-rpc-2 — that's a
separately-onboarded Foundation committee, see
`deploy/scripts/onboard-evm-quorum-node.sh`). Every node built by this
package is a **read / relay / witness** participant: it holds its own copy
of chain state (or delegates to the public RPC), serves `rope_*`/`eth_*`
JSON-RPC, and joins the Testimony gossip mesh — which is exactly what a
third-party operator or a Databox device should run.

## Choose a profile

| | `full` | `witness` |
|---|---|---|
| Local Reth (EVM chain data) | Yes — synced via Engine-API follower | No — delegates to `erpc.datachain.network` |
| Disk | 100 GB+ SSD recommended, grows over time | A few GB |
| CPU / RAM | 4 vCPU / 8 GB+ recommended | 1-2 vCPU / 1-2 GB is enough |
| Good fit for | Your own VPS/VM, a serious Databox tier | databox.network entry-level hardware, small VMs, Raspberry-Pi-class devices |
| `rope-node` mode | `relay` | `validator` |

Both profiles: no quorum keys, no committee membership, safe to run
anywhere without any coordination with the Foundation.

## Quick start

```bash
git clone https://github.com/KazeONGUENE/rope.git
cd rope/deploy/node-package    # or wherever this package lives in your checkout

# Witness / Databox profile (lightweight):
sudo ./install.sh --profile witness --name my-first-databox

# Full node profile (own copy of chain state):
sudo ./install.sh --profile full --name my-vps-node
```

That's it — `install.sh` installs OS packages, creates a dedicated
`rope` system user, fetches/builds the binaries, writes systemd units,
and starts everything.

## What install.sh does

1. Installs OS packages (`build-essential`, `libssl-dev`, `ufw`, ...).
2. Creates a system user (`rope` by default) with no login shell.
3. Lays out `/opt/datachain-rope/{data,bin}` (override with `--data-dir` /
   `--bin-dir` / `--install-dir`).
4. Installs the canonical `genesis.json` (chainId `271828`) — the exact
   genesis used by the production fleet, so your node's history matches
   theirs from block 0.
5. **`full` profile only:** downloads a prebuilt Reth `v1.11.2` binary for
   your architecture (x86_64 / aarch64), or builds it from source
   (`asm_keccak,jemalloc` features, matching what the production fleet
   runs) if no prebuilt binary is available.
6. Builds `rope` (the CLI/node binary) and, for the `full` profile,
   `rope-engine-driver` (the Engine-API follower that keeps your local
   Reth in sync) — both from source, since these are Datachain Rope's own
   crates.
7. Renders your node config from a template (`config/rope-full.toml.tmpl`
   or `config/rope-witness.toml.tmpl`) and your systemd units, then
   `daemon-reload`s.
8. Opens `9000/tcp` (rope-node P2P) in `ufw` if it's installed. RPC and
   Engine-API ports are bound to `127.0.0.1` only — nothing public unless
   you change it yourself.
9. Starts the services (`reth-rope` + `rope-evm-follower` for `full`;
   `datachain-rope-node` always).

## Options

```
--profile full|witness     required
--name <name>              node display name (default: hostname)
--data-dir <path>          default /opt/datachain-rope/data
--bin-dir <path>           default /opt/datachain-rope/bin
--install-dir <path>       default /opt/datachain-rope
--user <name>              service user (default: rope)
--rope-repo <git-url>      default https://github.com/KazeONGUENE/rope.git
--rope-ref <branch/tag>    default main
--deployer-wallet <addr>   optional, for the [deployer] attestation block
--operator-name <name>     optional
--operator-email <email>   optional
--operator-country <cc>    optional, ISO-3166 alpha-2
--skip-firewall            don't touch ufw
```

If you run `install.sh` from inside an already-checked-out
`datachain-rope` monorepo, it builds in place instead of re-cloning.

## Verifying it worked

```bash
# full profile: is Reth catching up to the public head?
curl -s -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
  http://127.0.0.1:8595
curl -s -X POST -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}' \
  https://erpc.datachain.network

# any profile: is rope-node up?
journalctl -u datachain-rope-node.service -f
```

A fresh `full` node starts from genesis (block 0) and mirrors forward at
whatever rate `rope-evm-follower.service` can pull batches from
`erpc.datachain.network` — for the current chain height (~3.45M blocks)
expect this to take a while on a fresh datadir. Progress is visible via
`journalctl -u rope-evm-follower.service -f`.

## Registering on the Global Databox Network (optional)

Any node — `full` or `witness` — can self-register at
[dcscan.io/databoxes](https://dcscan.io/databoxes) so it shows up in the
public registry map and stats. Registration is a signed, on-chain-anchored
event; heartbeats are lightweight liveness pings (not anchored).

```bash
# Use a dedicated low-value wallet for this — not your treasury key.
/opt/datachain-rope/scripts/register-databox.sh \
  --private-key 0xYOUR_KEY \
  --name "my-first-databox" \
  --type databox \
  --region eu-west \
  --city Paris --country FR --lat 48.85 --lon 2.35

sudo systemctl enable --now databox-heartbeat.timer
```

Valid `--type` values: `databox`, `rpc_slot`, `witness`, `community_node`
(plus four EDC-role types documented in
`crates/rope-explorer/src/databox_registry.rs` for Ecosystem Deployment
Console nodes). Requires [Foundry's `cast`](https://getfoundry.sh) for
EIP-191 signing:

```bash
curl -L https://foundry.paradigm.xyz | bash && foundryup
```

## Uninstalling

```bash
sudo ./uninstall.sh              # stops services, removes units, keeps data
sudo ./uninstall.sh --purge      # also deletes chain data, config, keys
```

## Notes for Databox hardware operators

- The `witness` profile is what `databox.network` hardware tiers should
  ship with by default — no local Reth, minimal disk/CPU.
- If your device is powerful enough to hold the full chain locally (SSD +
  4GB+ RAM), the `full` profile gives you an independent copy of state
  that doesn't depend on `erpc.datachain.network` being up at request
  time — strictly better for censorship-resistance and offline-capable
  use cases, at the cost of more disk/CPU.
- This node type is registered as `NodeKind::Seeder` in
  `rope-deployer`/`rope-cli`'s `rope deploy <provider> databox` flow when
  Foundation-provisioned; this package is the equivalent **self-hosted**
  path for hardware you already own.

## Known follow-ups (not blocking, tracked for future work)

- Reth's own devp2p discovery (`--bootnodes` / `enode://`) is intentionally
  not configured — sync happens entirely through the Engine-API follower
  against the public RPC, matching how the production fleet itself
  operates today.
