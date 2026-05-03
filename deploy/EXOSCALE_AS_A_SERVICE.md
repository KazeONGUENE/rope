# Exoscale-as-a-Service for Datachain Rope third-party deployments

**Author:** Datachain Foundation
**Status:** MVP scaffold landed (Phase D), 2026-05-03 — `rope-deployer` crate
compiles, unit tests pass, and the `rope deploy` CLI dispatches end-to-end
through `local`, `exoscale` (dry-run), and `digitalocean` (dry-run) providers.
**Scope:** Let third parties (community node operators, federations, app
builders, NaturaProof / Tanastok / Datawallet+ users) deploy Datachain
Rope nodes onto cloud infrastructure WITHOUT having to bring their own
cloud account.

---

## Why "Exoscale first"

| Property | Why it matters |
|---|---|
| Swiss / EU sovereign cloud | Aligns with the European regulatory posture of the Datachain Foundation |
| Mature CEL-policy IAM | We can mint per-tenant restricted API keys safely |
| Private networks per tenant | Strong network isolation between unrelated deployments |
| Public Compute API + libraries | Easy to call from Rust or curl |
| Stable list of zones | `ch-gva-2`, `ch-dk-2`, `de-fra-1`, `at-vie-1`, etc. |
| Foundation already has an account | `https://portal.exoscale.com/u/datachain-foundation/compute/instances` |

DigitalOcean parity is Phase E. The provider trait is designed so the
DO adapter is a drop-in.

---

## Architectural overview

```
                       ┌────────────────────────────────────────┐
                       │   Datachain founder / Foundation       │
                       │   - Holds master Exoscale Org key      │
                       │   - Pays the underlying cloud bill     │
                       │   - Bills tenants in DC FAT (escrow)   │
                       └────────────────────┬───────────────────┘
                                            │ provisions
                                            ▼
                       ┌────────────────────────────────────────┐
                       │   rope-deployer (Phase D MVP)          │
                       │   Rust HTTP service running on BLUE    │
                       │   Listens on https://deployer.datachain.network │
                       │                                        │
                       │   For each tenant request:             │
                       │   1. Verify Datawallet+ DID claim      │
                       │   2. Mint IAM key scoped to tenant     │
                       │   3. Create private network            │
                       │   4. Provision baked rope-node image   │
                       │   5. Tag with tenant DID + return PeerID │
                       └────────────────────┬───────────────────┘
                                            │ Exoscale Compute API
                                            ▼
            ┌────────────┐     ┌────────────┐     ┌────────────┐
            │ tenant A   │     │ tenant B   │     │ tenant C   │
            │ priv-net   │     │ priv-net   │     │ priv-net   │
            │ ┌────────┐ │     │ ┌────────┐ │     │ ┌────────┐ │
            │ │  vm 1  │ │     │ │  vm 1  │ │     │ │  vm 1  │ │
            │ └────────┘ │     │ ├────────┤ │     │ └────────┘ │
            │            │     │ │  vm 2  │ │     │            │
            │            │     │ └────────┘ │     │            │
            └────────────┘     └────────────┘     └────────────┘
                  │                  │                  │
                  └──────────────────┼──────────────────┘
                                     │ libp2p (Datachain Rope mesh)
                                     ▼
                              BLUE / GREEN / rpc-1 / rpc-2
                              (foundation master nodes)
```

---

## Tenant-isolation model

### Single Foundation Exoscale account, multi-tenant inside

We use **one** Exoscale organization (the existing
`datachain-foundation` org) as the cloud-billing entity. Tenants do
NOT get their own Exoscale accounts. Isolation is enforced at four
layers:

| Layer | Mechanism |
|---|---|
| **L1 — IAM** | Each tenant deployment is provisioned with a freshly-minted IAM API key whose CEL policy restricts it to `instance.labels.tenant_did == "<their DID>"` and a fixed list of `operation`s (start/stop/get/list of their own instances; nothing else). The tenant-side key is what `rope-deployer` returns to the user — they can lifecycle-manage their own VMs, but not anyone else's. |
| **L2 — Private network** | Each tenant gets a dedicated Exoscale Private Network (per-zone). All their VMs join only that network. Cross-tenant traffic is impossible at the L2 layer. |
| **L3 — Tags** | Every resource is tagged with `tenant_did=<DID>`, `tenant_email=<email>`, `deployed_by=rope-deployer`, `chain_id=271828`, `node_kind=<witness|rpc|community>`. This makes billing, audit, and emergency cleanup straightforward. |
| **L4 — Quota** | Per-DID hard cap on concurrent instances (default: 3 small or 1 medium). Tracked in `rope-deployer`'s own state DB, enforced before each `create-instance` call. |

### Sample CEL policy minted per tenant

```yaml
default-service-strategy: deny
services:
  compute:
    type: rules
    rules:
      - expression: |
          operation in ['list-instances', 'get-instance', 'list-private-networks']
          && resources.instance.labels.tenant_did == 'did:datachain:0x...'
        action: allow
      - expression: |
          operation in ['stop-instance', 'start-instance', 'reboot-instance']
          && resources.instance.labels.tenant_did == 'did:datachain:0x...'
        action: allow
      - expression: |
          operation == 'create-instance'
          && parameters.labels.tenant_did == 'did:datachain:0x...'
          && parameters.template_id == '<rope-node baked template UUID>'
          && parameters.private_network.id == '<this tenant's private network UUID>'
        action: allow
      - expression: 'true'
        action: deny
```

This means a leaked tenant key can:
- list / get / start / stop / reboot the tenant's own instances
- create a NEW instance ONLY if it uses the official baked rope-node
  template AND joins the tenant's private network AND is tagged with
  the tenant's DID

It cannot:
- touch any other tenant's instance
- read other tenants' private networks
- modify IAM, billing, DNS, S3 buckets, etc.
- escape into the foundation org's main resources

---

## Baked rope-node template

`rope-deployer` creates a single Exoscale "Custom Template" containing:
- Ubuntu 24.04 base
- Reth v1.11.2 binary at `/usr/local/bin/reth`
- `rope` binary at `/usr/local/bin/rope` (built from this repo)
- Systemd units for `reth-rope.service` + `datachain-rope.service`
- Pre-baked `master-nodes.toml` (foundation registry)
- A `cloud-init` script that:
  - Reads instance labels for `tenant_did`, `node_kind`
  - Generates a fresh node Ed25519 keypair under `/home/ubuntu/.rope/keys/`
  - Writes `/home/ubuntu/datachain-rope/deploy/config/rope-deployer.toml`
    with the tenant's deployer attestation (signed by the FOUNDATION
    using a per-tenant attestation flow — the tenant can re-sign with
    their own ONCHAINID later)
  - Joins the libp2p mesh using BLUE as bootstrap
  - Reports its NodeId back to `rope-deployer` so it can be added to
    `member_nodes` in master-nodes.toml on the next sync

**Image rotation:** The template is rebuilt nightly from `main` by a
GitHub Actions job that calls `rope-deployer`'s `/v1/templates/build`
endpoint (admin-only).

---

## CLI flow (user perspective)

```bash
# 1. User generates their Datawallet+ DID + ONCHAINID (one time)
#    via the existing Datawallet+ mobile/web app, or:
rope identity init-founder --output ~/.rope/me.key   # for individual deployers
# (For org deployers we will add `rope identity init-org` in a follow-up
# that registers the org's incorporation number + address into ONCHAINID.)

# 2. User deploys
rope deploy exoscale community-node \
  --region ch-gva-2 \
  --size medium \
  --identity ~/.rope/me.did.json

#    →  rope CLI POSTs to https://deployer.datachain.network/v1/provision
#       with a payload signed by the user's DID key.
#    →  rope-deployer:
#         - Verifies the signature against the DID claim
#         - Looks up DC FAT escrow balance (tenant must have prepaid)
#         - Mints scoped IAM key
#         - Creates private network if missing
#         - Calls Exoscale create-instance with tenant labels
#         - Waits for cloud-init to report NodeId
#         - Adds entry to master-nodes.toml under [[member_nodes]]
#         - Returns:
#               { "node_id": "<hex>", "ip": "...", "ssh_user": "ubuntu",
#                 "ssh_key": "<embedded one-time key>", "tenant_iam_key": "..." }

# 3. User manages
rope deploy list                      # show all my instances
rope deploy stop --node-id <hex>      # graceful stop (refunds remaining FAT)
rope deploy reboot --node-id <hex>    # …
```

---

## `rope-deployer` service — MVP scope

The MVP service ships in this commit as a Rust scaffold under
`crates/rope-deployer/`. It is deliberately small:

| Endpoint | Implementation status |
|---|---|
| `GET  /health` | landed (`rope_deployer::api::health`) |
| `GET  /providers` | landed (`rope_deployer::api::providers`) |
| `POST /v1/instances` | landed in-process (`rope_deployer::api::provision`); Exoscale + DO are dry-run until `EXOSCALE_API_KEY` / `DIGITALOCEAN_TOKEN` are set + live REST calls are wired |
| `GET  /v1/instances/:tenant_did` | landed (`rope_deployer::api::list_instances`) |
| `POST /v1/instances/:tenant_did/:id/stop` | provider-level (`CloudProvider::stop`) — HTTP wiring follow-up |
| `DELETE /v1/instances/:tenant_did/:id` | provider-level (`CloudProvider::destroy`) — HTTP wiring follow-up |

The `provision` endpoint is designed around a `CloudProvider` trait so
the DigitalOcean adapter (Phase E) plugs in without changing the HTTP
surface.

### Provider trait

```rust
#[async_trait]
pub trait CloudProvider: Send + Sync {
    async fn provision(&self, req: &ProvisionRequest) -> Result<ProvisionResponse>;
    async fn destroy(&self, instance_id: &str) -> Result<()>;
    async fn list(&self, tenant_did: &str) -> Result<Vec<InstanceInfo>>;
    async fn stop(&self, instance_id: &str) -> Result<()>;
}
```

Each adapter is a thin wrapper around the provider's REST API. We do
NOT use third-party SDKs — direct `reqwest` calls keep the dependency
surface small and audit-friendly.

---

## Operational runbook (foundation side)

### One-time setup (do this ONCE per foundation)

1. Log into Exoscale at <https://portal.exoscale.com/u/datachain-foundation>.
2. Create an **Organization Master IAM key** with a policy allowing
   ALL operations on `compute`, `iam`, `dns`. Save it to `~/.exoscale/master.key`
   on the BLUE VPS, restricted to root:600.
3. Bake the first version of the rope-node template:
   ```bash
   ssh rope-vps 'sudo /usr/local/bin/rope-deployer admin bake-template --provider exoscale'
   ```
4. Set up the `rope-deployer` systemd unit (`deploy/rope-deployer.service`)
   with `EnvironmentFile=/etc/rope-deployer.env` containing:
   ```
   EXOSCALE_API_KEY=<master-key-id>
   EXOSCALE_API_SECRET=<master-key-secret>
   EXOSCALE_ZONE_DEFAULT=ch-gva-2
   EXOSCALE_TEMPLATE_ID=<UUID of baked template>
   DEPLOYER_HTTP_ADDR=127.0.0.1:7700
   DEPLOYER_DB=/var/lib/rope-deployer/state.db
   ```
5. Add nginx vhost `deployer.datachain.network` proxying to `127.0.0.1:7700`.
6. Add UFW rule: only allow port 7700 from localhost.

### Per-tenant operations

| Action | Command |
|---|---|
| List a tenant's instances | `rope-deployer admin instances --tenant did:datachain:0x...` |
| Force-destroy a tenant | `rope-deployer admin destroy-tenant --tenant did:datachain:0x... --reason "<...>"` |
| Rotate a tenant's IAM key | `rope-deployer admin rotate-key --tenant did:datachain:0x...` |
| Pause new provisioning | `rope-deployer admin pause` |
| Set per-tenant quota | `rope-deployer admin quota --tenant did:datachain:0x... --max-instances 5 --max-size large` |

---

## Billing model (sketch — implementation Phase D+1)

The Foundation pre-pays Exoscale for capacity. Tenants pre-pay the
Foundation in **DC FAT** by transferring tokens to a per-tenant
escrow contract. `rope-deployer` reads escrow balance before each
provision and refuses when insufficient.

Hourly drawdown (e.g. 100 FAT/hr for `medium`) is debited automatically
by an off-chain cron that records signed receipts onto Datachain Rope
under `rope_appendToLedger` (so the user has a personal-ledger trail
of charges).

When escrow runs to zero: instances are paused (state preserved 7 days,
then destroyed).

Pricing tiers (provisional, calibrated against Exoscale's own list price
plus a small Foundation operations margin):

| Size | Exoscale instance | DC FAT / hour | DC FAT / month (~720 h) |
|---|---|---|---|
| small  | tiny       (1 vCPU / 1 GB) | 5    | 3,600   |
| medium | medium     (4 vCPU / 8 GB) | 30   | 21,600  |
| large  | extra-large(8 vCPU / 32 GB)| 120  | 86,400  |

(These numbers are placeholders — finalize once Foundation governance
votes them in via `rope_governanceInfo`.)

---

## DigitalOcean parity (Phase E)

DigitalOcean's IAM doesn't have CEL-style scoping. The closest
equivalent is **Projects** (a Project is a billing-and-display
grouping). We will use:

- one foundation account
- one DigitalOcean Project per tenant
- a tenant-scoped DigitalOcean Personal Access Token via the
  newly-released DO IAM "team" preview, fenced by tag-based DOKS
  policies (use of `do:droplet:tag=tenant-<did>` ACL fragments)

The `CloudProvider` trait stays unchanged; only the implementation
differs.

---

## Open follow-ups (deferred from this MVP)

- [ ] Wire Datawallet+ DID resolution into `rope-deployer` (currently
      stubbed: it accepts any well-formed `did:datachain:...`).
- [ ] Implement DC FAT escrow contract + drawdown cron.
- [ ] Implement Exoscale REST calls (currently dry-run; signed by
      `EXOSCALE_API_KEY`).
- [ ] Build the actual baked rope-node template (`admin bake-template`
      command).
- [ ] Add a `rope deploy list` subcommand on the user side.
- [ ] DigitalOcean adapter (Phase E).
- [ ] Per-region pricing.

These are tracked as roadmap items in `master-node-governance.mdc`
under "Phase D follow-up" and "Phase E".
