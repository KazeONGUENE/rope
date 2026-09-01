# Cross-Platform Ecosystem Entity Provisioning - v1.1

**Status:** DRAFT v1.1 (2026-08-13)
**Owner:** Datachain Rope agent
**Consumers:** every project owner, databox operator, node operator, and ecosystem partner platform (Tanastok, DCSwap, NaturaProof, Syndicated, Datawallet+, Careaway, Alteros, and any future ecosystem project).
**Producers:** `rope-entity-provision` CLI (this spec), `rope-explorer` HTTP surface (`/v1/ecosystem/provision`, this spec).
**Interacts with:**
- `rope-node` - `rope_createPersonalLedger`, `rope_appendToLedger` on `EDC_REGISTRY_WALLET` (`0x000000000000000000000000000000000000ec01`) and per-entity strings.
- `rope-idp` - Datachain ID SSO (`https://id.datachain.network/.well-known/jwks.json`).
- `rope-edc` - Ecosystem Deployment Console (`console.datachain.network`) wizard delegation.
- **`rope-ecosystem-discovery.service`** (LIVE on rope-vps since 2026-08-13T~19:20Z per handover §31.9) - the deployed systemd unit that owns the overlay JSONL file at `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl`. **The provisioner shares this file** as its post-commit fast-path listing surface (see §7.7, §7.8). Runbook: `docs/ROPE_ECOSYSTEM_DISCOVERY_RUNBOOK.md`. Systemd unit: `deploy/rope-ecosystem-discovery.service`. Config: `/etc/rope-ecosystem-discovery.toml` (writes every `run_interval_secs`, default 900s).
- `rope-explorer` - `ecosystem_canonical.rs`, `ecosystem_overlay.rs` (loader), `databox_registry.rs`, ecosystem directory + card renderer. Reads the shared overlay file when `ECOSYSTEM_OVERLAY_PATH=/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` is set on `dc-explorer.service` (Phase B of §31.9, currently deferred pending 24 h soak).

---

## 0. Change log

| Date | Version | Change |
|---|---|---|
| 2026-08-13 | v1 (DRAFT) | First spec. Unifies the three provisioning flows that ship today (databox script, EDC wizard, Tanastok issuer scripts) behind a single crate `rope-entity-provision` plus a small HTTP surface, and formalises the shape of an "ecosystem entity" so `dcscan.io/ecosystem` can list projects, databoxes, nodes, and assets uniformly. |
| 2026-08-13 | v1.1 (DRAFT) | Explicitly link every provisioning service to the deployed `rope-ecosystem-discovery.service` (handover §31.9). The provisioner does not write its own overlay file; it appends to the JSONL file the discovery daemon already owns at `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl`. New §7.8 defines the coordination contract (path ownership, file locking, `discovered_by` enum extension, refresh cadence, disaster-recovery via re-scan). Phase 2 of the rollout plan now names `rope-ecosystem-discovery.service` as a hard pre-requisite. Compatibility matrix (§15) records that the discovery daemon needs no change to consume provisioner-authored rows once the `entity_provisioner` value is added to its writer contract. |

---

## 1. Purpose

Right now, "onboarding a new thing to the Datachain Rope" means one of three completely different procedures depending on what the thing is:

| Thing | Procedure today | Files |
|---|---|---|
| A community databox / RPC slot / testimony witness / community node | Run `deploy/node-package/scripts/register-databox.sh` on the box. Bash + `cast wallet sign` + curl. EIP-191 sig with domain `DCROPE-DATABOX-AUTH`. Server-side: `crates/rope-explorer/src/databox_registry.rs::register_databox` verifies, `rope_appendToLedger` on `0x...d003`. | `deploy/node-package/scripts/register-databox.sh`, `crates/rope-explorer/src/databox_registry.rs` |
| An IoT / analytics project (the EDC archetype: predictive maintenance, environmental monitoring, hybrid) | Nine-step wizard on `console.datachain.network`. Rust `Project` struct built up step-by-step. Deterministic wallet `blake3(edc-project:<id>)`. Genesis + lifecycle knots on the project's own string. Public card as `EcosystemProjectRegistered` on `0x...ec01`. | `crates/rope-edc/src/{types,registry,provision,grants}.rs`, `docs/ECOSYSTEM_DEPLOYMENT_CONSOLE_SPEC_V2.md` |
| A tokenized asset on a partner platform (Tanastok, NaturaProof, Syndicated, DCSwap pool, etc.) | Platform-specific issuer scripts. Tanastok: DCNFT deploy + ERC-3643 deploy + T-REX registry claim + manifest publish at `/api/v1/tokenized-assets`. DCSwap: `DCSwapFactory.createPair` + liquidity add + T-REX identity + claims. NaturaProof / Syndicated: even less automated. | `datachain-rope/docs/SPEC_TANASTOK_ENTITY_INTEGRATION_V1.md`, `handover-dcswap-redeployed-2026-02-26.mdc`, `handover-tanastok-tokenized-assets-for-dcscan-2026-03-30.mdc` |

These three flows do not share a signing domain, a wallet-derivation contract, a knot schema, a registry wallet, an authentication mechanism, or a listing surface. Each one had to re-solve "how do I prove the operator owns the thing", "how do I create a Quipu string for it", "how do I publish a card so dcscan can list it".

**This spec unifies all three under one crate + one HTTP surface + one on-chain contract**, without changing what those three flows publish. The unification is purely at the operator's point of contact: instead of picking one of three procedures, an operator runs one command (or one HTTP call) and gets a wallet, a DID, a Quipu string, a genesis knot, a public card, and (where applicable) a platform-specific listing tx. Existing bespoke scripts keep working during the transition; new integrations use the unified flow.

The result: `dcscan.io/ecosystem` can list every kind of ecosystem entity (project, databox, node, asset, contract, identity, cord) with the same UX, and a new partner platform can be onboarded by writing one adapter instead of a full stack.

---

## 2. Design goals + non-goals

### 2.1 Goals

1. **One command to onboard any entity.** `rope-entity-provision --kind <k> --name <n> ...` on the operator's machine, or `POST /v1/ecosystem/provision` from a partner integration, produces (wallet ⋀ DID ⋀ Quipu string ⋀ genesis knot ⋀ public card ⋀ platform tx-batch where applicable) in one atomic call. If any step fails, the whole call is undone (see §12 idempotency).
2. **Zero re-implementation** of the crypto primitives already in production (`rope_createPersonalLedger`, `rope_appendToLedger`, EIP-191 `personal_sign`, `did:web:datawallet.plus:<uuid>`, ONCHAINID claims, `EDC_REGISTRY_WALLET`, `DATABOX_LEDGER_WALLET`).
3. **Adapter pattern for partner platforms**, so adding Tanastok / DCSwap / NaturaProof / Syndicated / a new partner is a self-contained module rather than a change to the core provisioner.
4. **BYO-wallet OR deterministic-wallet.** The operator may bring their own Datawallet+ wallet as the entity's controller (preferred for real people / real organizations), or the service derives a deterministic per-entity wallet (needed for machines / bots / anonymous entities like community relays).
5. **Datachain ID SSO as primary auth**, EIP-191 as fallback. If the operator has a Datawallet+ account they authenticate once via `id.datachain.network` and provision N entities from that session. If they don't (fresh box, no user account), they can still sign each request individually with a plain EVM wallet.
6. **Public listing is a first-class effect**, not an afterthought. Every provisioned entity appears on `dcscan.io/ecosystem` within one refresh cycle (max 15 min per the overlay contract, near-instant for EDC-anchored cards).
7. **Idempotent + resumable.** Re-running the same request produces the same result. A partial failure can be retried without producing duplicate entities.

### 2.2 Non-goals

1. **Not a wallet-manager.** This service does not custody private keys long-term. Deterministic wallets are ephemeral in the process; if the operator wants persistent custody they use Datawallet+ or bring their own wallet.
2. **Not a replacement for the EDC wizard.** The EDC wizard is the correct UX for the nine-step IoT project archetype (KYC, node sizing, asset inventory, mutability policy, etc.). This service is what the EDC calls under the hood to produce the wallet + string + card. Existing EDC flows keep working; they just delegate to `rope-entity-provision`.
3. **Not a replacement for Tanastok/DCSwap/etc. issuer scripts.** Deploying a DCNFT or an ERC-3643 T-REX pair is out of scope. This service registers the entity in the ecosystem so it appears on dcscan; the partner platform still runs its own asset-issuance ceremony. Adapters translate between the two.
4. **Not a self-sovereign identity provider.** DIDs are attached (`did:web:datawallet.plus:<uuid>` or `did:dwp:...`), not minted. If the operator has no Datawallet+ account, the entity gets an `did:rope:entity:<wallet>` synthetic DID with a matching pointer knot; this is honest ("no verified human is attesting to this identity") rather than fake ("this box is Alice").
5. **Not a bridge.** Cross-chain state is out of scope. This service registers Datachain Rope entities. If a partner platform has cross-chain assets (Tanastok has an XDC bridge, DCSwap wraps FAT), the adapter for that platform handles the cross-chain call; the core provisioner only touches Rope.

---

## 3. Ground truth today (what already exists)

Compressed audit of the three existing flows, extracted from live code as of 2026-08-13.

### 3.1 Databox / node registration (bash + EIP-191)

- Script: `deploy/node-package/scripts/register-databox.sh` (81 lines).
- Signing: `cast wallet sign --private-key <hex> "$MESSAGE"`.
- Message: `DCROPE-DATABOX-AUTH\nregister\n{name}\n{type}\n{region}\n{timestamp}`.
- Server: `POST https://dcscan.io/api/v1/databoxes/register` handled by `crates/rope-explorer/src/databox_registry.rs::register_databox` (~200 LOC).
- Server-side steps:
  1. Verify EIP-191 sig with `k256::ecdsa::VerifyingKey::recover_from_prehash` (`recover_signer`).
  2. Compute `databox_id = "dbx-" + hex(keccak256(owner.lower() || "|" || name.lower()))[:16]`.
  3. Persist to `/opt/datachain-rope/databoxes.jsonl` (JSONL, atomic tmp+rename).
  4. Anchor `DataboxRegistered` knot on `DATABOX_LEDGER_WALLET` (`0x...d003`) via `rope_appendToLedger`.
  5. Return `{ id, owner_address, endpoint }` to the script.
- Heartbeat: separate `heartbeat-databox.sh` runs on a systemd timer every 5 min.
- Kinds supported: `databox`, `rpc_slot`, `witness`, `community_node`, `ingestion_gateway`, `storage_ledger`, `ai_agent_host`, `federation_validator` (8 total, from `DATABOX_TYPES` in `databox_registry.rs`).
- Frontend: `dcscan.io/databoxes` renders the JSONL.

### 3.2 EDC project provisioning (Rust wizard)

- Crate: `crates/rope-edc/*` (production, shipping today).
- Console: `console.datachain.network`, wallet-authed via Datachain ID.
- Wallet derivation: `crates/rope-edc/src/types.rs::project_wallet`:
  ```rust
  pub fn project_wallet(project_id: &str) -> String {
      let hash = blake3::hash(format!("edc-project:{project_id}").as_bytes());
      format!("0x{}", hex::encode(&hash.as_bytes()[..20]))
  }
  ```
- Identity binding: every project carries an `IdentityInfo` with `did` (Datawallet+ DID) + `onchainid` (ERC-3643 T-REX identity address) + KYC/KYB claim.
- String creation: `crates/rope-edc/src/registry.rs::ensure_ledger` calls `rope_createPersonalLedger` for the project wallet.
- Genesis + lifecycle knots: anchored on the project's own string via `rope_appendToLedger` (e.g. `ProjectSubmitted`, `ProjectDeploying`, `NodeProvisioned`, `AssetIngested`).
- Public card: anchored on `EDC_REGISTRY_WALLET` (`0x...ec01`, `EcosystemProjectRegistered`) so `dcscan.io/ecosystem` auto-lists.
- Node provisioning: `crates/rope-edc/src/provision.rs` calls `rope-deployer` with `ProvisionRequest { tenant_did, tenant_onchainid, ... }`.
- Node roles: `NODE_ROLES = ["ingestion_gateway", "storage_ledger", "ai_agent_host", "federation_validator"]`.

### 3.3 Tanastok asset issuance (mixed Rust + JS + manual)

- 198 assets live, per handover `handover-tanastok-tokenized-assets-for-dcscan-2026-03-30.mdc`.
- Per-asset ceremony:
  1. Deploy DCNFT (ERC-721) contract as immutable title deed. 1 token minted, `name()` = asset name.
  2. Deploy ERC-3643 (T-REX) contract as fractional-share security token. `name()` = "{Asset} Shares", `symbol()` = "{SYM}-S".
  3. Register the ERC-3643 in T-REX `IdentityRegistry` with claim topics {1, 2, 3, 4, 10, 99} issued by `DatawalletClaimIssuer`.
  4. Publish to Tanastok's manifest at `https://tanastok.io/api/v1/tokenized-assets`.
- ROPE-side integration: `crates/rope-node/src/entity_manifest.rs` polls the manifest every 5 min and materialises entities into the `LabelRegistry`. `crates/rope-explorer/src/entity_labels.rs` maps them to the `LABELS` map.
- Cross-referenced in dcscan address page via `dcscan.io/address/<addr>` fetching the manifest.
- **Not yet emitted as Quipu strings** (documented gap in `SPEC_TANASTOK_ENTITY_INTEGRATION_V1.md` §1). The strings-per-asset work is on the roadmap; this spec assumes it lands and defines how the provisioner would drive it uniformly.

### 3.4 Common gaps across all three

| Gap | Impact |
|---|---|
| No shared signing domain across the three flows | Every new flow re-invents replay-protection. Easy to accidentally accept a databox-register sig as an EDC project-create sig. |
| No shared wallet-derivation contract | Same operator ends up with 3 wallets for 3 things. Hard to link "this databox and this project have the same owner". |
| No unified DID attachment | Databox flow has no DID at all (bare wallet). EDC has full DID + ONCHAINID. Tanastok has an "issuer ONCHAINID" but no per-asset DID. |
| No shared ecosystem card schema | Databox JSONL has one shape, EDC card has another, Tanastok manifest a third. dcscan needs 3 different renderers. |
| No shared listing wallet | Databoxes → `0x...d003`, EDC projects → `0x...ec01`, Tanastok → external manifest. Consumers can't do "give me every ecosystem entity in one call". |
| Partner platforms have to re-implement the whole ceremony | NaturaProof and Syndicated have working platforms but their assets don't appear on dcscan because there is no simple "list my asset" API. |

The unification below closes each of these gaps additively (existing flows keep working; new flows use the unified path).

---

## 4. Target architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                          OPERATOR SURFACE                                    │
│                                                                              │
│  A) CLI on the box:                                                          │
│     rope-entity-provision --kind databox --name my-node-01 \                 │
│       --region eu-west --auth-mode wallet --private-key 0x...                │
│                                                                              │
│  B) HTTP from a partner platform:                                            │
│     POST https://dcscan.io/v1/ecosystem/provision                            │
│     Authorization: Bearer <DATACHAIN_ID_JWT>                                 │
│     Content-Type: application/json                                           │
│     { "kind": "asset", "name": "Kibali Gold Mine",                           │
│       "adapter": "tanastok", "adapter_payload": {...} }                      │
│                                                                              │
│  C) EDC wizard (existing, delegates to this service under the hood):         │
│     console.datachain.network → nine-step wizard → server calls              │
│     rope_entity_provision::provision_entity(...)                             │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                    crates/rope-entity-provision                              │
│                       (new crate, this spec)                                 │
│                                                                              │
│  1. Auth: verify Datachain ID JWT OR EIP-191 sig (DCROPE-ENTITY-PROV)        │
│  2. Wallet derivation: BYO from Datawallet+ OR deterministic per-entity      │
│  3. DID attachment: from JWT.did OR did:rope:entity:<wallet>                 │
│  4. Idempotency: hash(kind, canonical_name, controller) → entity_id;         │
│     re-runs return the existing entity                                       │
│  5. Preflight: run the platform adapter's `preflight()` (validates payload,  │
│     checks external-service reachability, reserves any external IDs)         │
│  6. On-chain sequence (atomic, one-shot rollback on partial failure):        │
│     a. rope_createPersonalLedger(entity_wallet)                              │
│     b. rope_appendToLedger(entity_wallet, EntityGenesis knot)                │
│     c. rope_appendToLedger(REGISTRY_WALLET, EcosystemEntityRegistered card)  │
│     d. adapter.commit() (platform-specific tx-batch)                         │
│  7. Overlay-write handoff (fast-path listing; see §7.8):                     │
│     append OverlayEntry{discovered_by:"entity_provisioner", ...} to          │
│     /var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl                │
│     (file owned by rope-ecosystem-discovery.service, shared writer)          │
│  8. Return { entity_id, wallet, did, genesis_knot, card_knot, adapter_out }  │
└─────────────────────────────────────────────────────────────────────────────┘
                    │                                    │
                    │                                    │  (7) shared-file
                    │                                    ▼  append (§7.8)
                    │                    ┌─────────────────────────────────────┐
                    │                    │  rope-ecosystem-discovery.service   │
                    │                    │  (LIVE on rope-vps, handover §31.9) │
                    │                    │                                     │
                    │                    │  Owns: /var/lib/rope-ecosystem-     │
                    │                    │        discovery/ecosystem-         │
                    │                    │        overlay.jsonl                │
                    │                    │  Refresh: every run_interval_secs   │
                    │                    │           (default 900s)            │
                    │                    │  Coexists with provisioner rows via │
                    │                    │  dedup-by-lowercase-id (first wins) │
                    │                    └─────────────────────────────────────┘
                    │                                    │
                    │  (6a-6c) rope-node                 │  file read
                    │                                    │
                    ▼                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                          ADAPTER PLUGINS                                     │
│                                                                              │
│  builtin/databox.rs     - wraps existing databox_registry::register_databox  │
│  builtin/edc_project.rs - wraps existing rope_edc::registry::anchor_public_  │
│                           card + provision.rs                                │
│  builtin/generic.rs     - no external platform; just anchors on Rope         │
│  tanastok/asset.rs      - calls tanastok.io/api/v1/assets/register           │
│                           + optional T-REX / DCNFT deploy proxies            │
│  dcswap/pool.rs         - calls dcswap.net/v1/pools/register                 │
│                           + optional Router.createPair proxy                 │
│  naturaproof/verify.rs  - calls naturaproof.com/api/v1/claims/register       │
│  syndicated/investment.rs - calls syndicated.ltd/api/v1/vehicles/register    │
│  edc/node.rs            - calls existing rope-edc provision.rs               │
└─────────────────────────────────────────────────────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                       LISTING SURFACES (existing)                            │
│                                                                              │
│  rope-node                    Quipu strings per entity + registry cards      │
│  rope-explorer                /ecosystem, /databoxes, /address, /string      │
│    └─ ecosystem_overlay.rs    Loader reads /var/lib/rope-ecosystem-          │
│                               discovery/ecosystem-overlay.jsonl when         │
│                               ECOSYSTEM_OVERLAY_PATH is set (Phase B of      │
│                               §31.9, deferred pending 24 h soak)             │
│  rope-ecosystem-discovery     Overlay writer (LIVE §31.9); on-chain          │
│                               scanner emits `discovered_by:"onchain"`;       │
│                               provisioner appends `entity_provisioner`.      │
│                               Both coexist per §7.8.                         │
│  ecosystem_canonical.rs       Operator's hand-curated list (unchanged)       │
│  dcscan.io frontend           Renders all sources with precedence:           │
│                               EDC > canonical > overlay (dedup by id,        │
│                               entity-prov rows win over on-chain-scanner     │
│                               rows for the same id per §7.8)                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

The key insight: the provisioner writes **the same shape** on-chain regardless of platform. The platform adapter handles what's platform-specific (mint a DCNFT, create a DCSwap pool, register with a NaturaProof verifier). Every entity gets:
- One wallet.
- One DID.
- One Quipu string.
- One genesis knot on that string.
- One public card on the shared `EDC_REGISTRY_WALLET` (via the existing `EcosystemProjectRegistered` schema, which becomes `EcosystemEntityRegistered` with a `kind` discriminator - backward-compatible additive change).

That last point is what lets `dcscan.io/ecosystem` list "every kind of thing" uniformly.

---

## 5. Entity kind taxonomy

Seven canonical kinds, chosen to cover every existing flow + leave room for future partners.

| Kind | Description | Wallet derivation default | Registry wallet | Adapter |
|---|---|---|---|---|
| `project` | An IoT / analytics project (EDC archetype: predictive maintenance, environmental monitoring, hybrid). | Deterministic `blake3("edc-project:" || id)` | `EDC_REGISTRY_WALLET` (`0x...ec01`) | `builtin/edc_project.rs` |
| `databox` | A community databox / storage seeder. | Deterministic `blake3("databox:" || owner || "|" || name)` OR BYO | `DATABOX_LEDGER_WALLET` (`0x...d003`) | `builtin/databox.rs` |
| `node` | An operator-run node (RPC slot, witness, community node, EDC ingestion gateway / storage ledger / AI agent host / federation validator). | Same as `databox` | `DATABOX_LEDGER_WALLET` | `builtin/databox.rs` (databox and node share the same ledger; distinguished by `entity_kind` field) |
| `asset` | A real-world tokenized asset (Tanastok DCNFT, Syndicated investment vehicle, NaturaProof biodiversity certificate). | BYO issuer wallet + deterministic per-asset `blake3("asset:" || platform || ":" || issuer || ":" || slug)` | `EDC_REGISTRY_WALLET` | Platform adapter |
| `contract` | A DCR-20 / ERC-3643 / DCNFT contract deployed on Rope. | The contract's own EVM address | `EDC_REGISTRY_WALLET` | Platform adapter (typically follows an `asset`) |
| `identity` | An organization or team (issuer, DAO, foundation). Not a person - persons stay in Datawallet+. | BYO controller wallet + `did:web:...` or `did:dwp:...` | `EDC_REGISTRY_WALLET` | `builtin/generic.rs` |
| `cord` | A federation cord (multi-project index, e.g. a regulator's view of all their supervised projects). | Deterministic `blake3("cord:" || slug)` | `EDC_REGISTRY_WALLET` | `builtin/generic.rs` |

These map 1:1 onto the existing `StringKind` enum in `crates/rope-core/src/personal_ledger.rs` (`Wallet`, `Contract`, `Asset`, `Did`, `Cord`) plus the two operational buckets from `databox_registry.rs::DATABOX_TYPES`. No new `StringKind` variants are needed; the additional distinction (`databox` vs `node` vs `project` all being `StringKind::Wallet` at the Quipu layer) lives in the card metadata rather than the string type.

### 5.1 Kind → StringKind mapping (for the `rope_createPersonalLedger` call)

| kind | StringKind | Rationale |
|---|---|---|
| `project` | `Wallet` | Uses `project_wallet(id)` as its controller address; the string is per-wallet. |
| `databox` / `node` | `Wallet` | Uses either BYO or deterministic wallet. |
| `asset` | `Asset` | Distinct from `Wallet` because assets aren't controlled by an EOA - they're controlled by their issuer's wallet + T-REX identity. `id_bytes = keccak256(canonical_asset_uri)`. |
| `contract` | `Contract` | The contract address IS the id. `id_bytes = <contract_address>` left-zero-padded to 32 bytes. |
| `identity` | `Did` | The identity's DID address. `id_bytes` = the ONCHAINID address or Datawallet+ DID hash. |
| `cord` | `Cord` | Uses the deterministic cord wallet. |

The provisioner passes the correct `kind` string to `rope_createPersonalLedger` (which reads it as the optional second parameter per Quipu Canon v1.2). Non-`Wallet` kinds require the v1.2.1 API extension; if the deployed rope-node predates v1.2.1, the provisioner falls back to `Wallet` and records the intended `kind` in the entity card metadata (documented graceful degradation, not a bug).

---

## 6. Signing domain + authentication model

### 6.1 The signing domain

**New canonical domain: `DCROPE-ENTITY-PROV`.**

Message format (mirrors the existing `DCROPE-DATABOX-AUTH` and `DATACHAIN-ID-AUTH` shapes so tooling can be reused):

```
DCROPE-ENTITY-PROV
{action}
{entity_kind}
{canonical_name}
{controller_address_lowercase}
{unix_timestamp}
{payload_hash}
```

Where:
- `{action}` ∈ `{ provision | update | deprecate }`
- `{entity_kind}` ∈ the seven kinds from §5
- `{canonical_name}` is the normalised entity name (see §7.2)
- `{controller_address_lowercase}` is the operator's EVM address (either their Datawallet+ primary_address or a plain-EVM wallet)
- `{unix_timestamp}` is seconds-since-epoch, ±300 s freshness window (matching `DATABOX_AUTH_WINDOW_SECS`)
- `{payload_hash}` is `hex(keccak256(canonical_json(request_body_without_signature)))` - commits the whole payload so a valid sig can't be reused with a different body

Signed via EIP-191 `personal_sign` (same construction as `databox_registry::verify_signature` and `walletsig::verify_wallet_signature`).

**Why a new domain and not `DCROPE-DATABOX-AUTH` or the EDC domain:**
- Domain separation is the whole point of the tagging scheme (per Quipu Canon v1.1 §7). A signature that was valid for "register a databox" MUST NOT be valid for "provision a Tanastok asset", or an attacker who observes a databox-register sig on the wire could replay it as an asset-registration.
- The existing domains stay valid for their existing flows so no legacy tooling breaks.

### 6.2 Two authentication modes

**Mode A: Datachain ID SSO (preferred for real people / real organizations).**

- Operator logs into `id.datachain.network` (once per session, browser or CLI OAuth device flow).
- Gets back a JWT with `sub` (Datawallet+ UUID), `did` (`did:web:datawallet.plus:<uuid>`), `primary_address` (their default wallet), `wallets[]`, `amr` (`pwd` or `wallet_signature`).
- Provisioner accepts `Authorization: Bearer <jwt>` in the HTTP surface. CLI reads it from `~/.config/rope/id-token.json` or the `DATACHAIN_ID_TOKEN` env var.
- **No per-request signature needed.** The JWT + freshness + audience check IS the auth.
- The entity's `controller_address` defaults to `jwt.primary_address` unless the request overrides it (in which case the override wallet must be in `jwt.wallets[]`, verified by wallet-type match).
- Best UX for a person onboarding 10 databoxes: one login, ten one-line CLI commands.

**Mode B: Wallet EIP-191 (fallback for machines / no-account operators).**

- Operator supplies `--private-key <hex>` to the CLI or signs the request body themselves and includes `signature` + `timestamp` in the JSON body.
- Provisioner reconstructs the message (§6.1), verifies via `k256::ecdsa::VerifyingKey::recover_from_prehash`, checks freshness, matches recovered address against the claimed `controller_address`.
- Same construction as `databox_registry::verify_signature` - reuse the existing helper.
- No DID unless the operator explicitly supplies one. If none, entity gets `did:rope:entity:<wallet>`.

Both modes produce the same server-side authorization decision: "an operator holding this private key is asking to provision this entity". The difference is user experience, not privilege.

### 6.3 Datachain ID mode implicitly grants a DID

If Mode A is used, `jwt.did` becomes the entity's DID (`did:web:datawallet.plus:<uuid>`), and the entity card carries a "verified operator" pill on dcscan.

If Mode B is used, the entity's DID is `did:rope:entity:<wallet>` (a self-sovereign but unverified DID), and the card carries no "verified operator" pill. This is honest: we're not going to lie about a fresh machine wallet being tied to a real person.

Operators using Mode B can later run `rope-entity-provision update --kind <k> --entity-id <id> --attach-did <did>` after registering with Datawallet+ to upgrade the entity's DID.

---

## 7. Wallet + DID + string provisioning

### 7.1 Wallet derivation contract

Two paths, chosen by the caller:

**BYO (bring your own):**
- The caller specifies `--controller-address 0x...` (CLI) or `"controller_address": "0x..."` (HTTP body).
- The provisioner uses that address as the entity's wallet.
- Requires that the operator's Mode A `jwt.wallets[]` includes this address, OR that Mode B EIP-191 sig recovers to it.
- Preferred for `identity` and `asset` kinds where the controller is a real party who wants their own custody.

**Deterministic (default):**
- The provisioner computes `entity_wallet = 0x + hex(blake3("<domain>:" || <identifier>).as_bytes()[..20])`.
- Per-kind domain string:
  - `project`: `edc-project:<project_id>` (matches existing `project_wallet` in `rope-edc/src/types.rs`, backward-compat)
  - `databox`, `node`: `databox:<owner_lower>|<name_lower>` (matches existing `compute_databox_id` in `databox_registry.rs`, backward-compat)
  - `asset`: `asset:<platform>:<issuer_lower>:<slug_lower>`
  - `contract`: N/A (contract address IS the wallet)
  - `identity`: `identity:<slug_lower>` (only for organizations without a wallet)
  - `cord`: `cord:<slug_lower>`
- Preferred for `databox`, `node`, `project`, `cord`, and `asset` when the platform doesn't already own a wallet.

Both paths converge on a lowercase 20-byte hex address that becomes the entity's Quipu string handle.

**Idempotency:** same (kind, canonical_name, controller) always produces the same entity_wallet (deterministic path) OR the same explicit wallet (BYO path). Re-running the same `provision` call is safe - see §12.

### 7.2 Canonical name normalisation

To make `blake3` inputs stable and to prevent trivial duplicates ("My Node" vs " my node "):

```rust
fn canonical_name(raw: &str) -> Result<String, ValidationError> {
    let trimmed = raw.trim();
    if trimmed.len() < 3 || trimmed.len() > 128 {
        return Err(ValidationError::NameLength);
    }
    // Lowercase, collapse internal whitespace to single space
    let lowered = trimmed.to_lowercase();
    let collapsed: String = lowered
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // Reject control chars, NULs, direction-control chars
    if collapsed.chars().any(|c| c.is_control() || c == '\u{FEFF}') {
        return Err(ValidationError::NameChars);
    }
    Ok(collapsed)
}
```

Used both for the `blake3` input (deterministic wallet) and for the entity's `slug` (URL-safe id used in cards + URLs).

### 7.3 DID attachment

| Auth mode | DID |
|---|---|
| Mode A (SSO), JWT has `did` | `did` from JWT verbatim (e.g. `did:web:datawallet.plus:bf92baf8-ecd8-...`) |
| Mode A, JWT has no `did` (edge case, older JWT) | Fall back to synthetic `did:rope:entity:<entity_wallet>` |
| Mode B (EIP-191 sig) | `did:rope:entity:<entity_wallet>` unless caller supplies `--did <did>` explicitly |
| Caller-supplied `--did` | Must resolve to a document that points back at the entity_wallet (verification deferred to a follow-up rule; today just recorded) |

The DID is stored in the entity card's `identity` block and is included in the genesis knot metadata. Follow-up work (out of scope for v1): resolve caller-supplied DIDs and verify they belong to the operator; today they're accepted verbatim and displayed on the card without a "verified" badge.

### 7.4 String creation

Single RPC call to `rope_createPersonalLedger` with the derived (or provided) wallet address:

```json
{
  "jsonrpc": "2.0", "id": 1,
  "method": "rope_createPersonalLedger",
  "params": ["<entity_wallet>", { "kind": "<StringKind>" }]
}
```

- The second parameter (kind hint) is optional per Quipu Canon v1.2 - the node returns success (`2001 Ledger already exists`) if the string is already there, so this call is idempotent.
- If the deployed rope-node doesn't accept the second parameter (predates v1.2.1), fall back to the one-arg form and rely on the default `Wallet` kind. This will be logged as `warn!("rope-node lacks kind hint, string will be kind=wallet regardless of intent={intent}")` and recorded in the entity card metadata as `"quipu_kind_intent": "asset"` etc., so the discrepancy is auditable rather than hidden.

### 7.5 Genesis knot

Immediately after string creation, anchor the entity's genesis knot on its own string:

```json
{
  "jsonrpc": "2.0", "id": 2,
  "method": "rope_appendToLedger",
  "params": ["<entity_wallet>", {
    "interaction_type": "EntityGenesis",
    "description": "<canonical_name>",
    "metadata": {
      "entity_kind": "<kind>",
      "canonical_name": "<canonical_name>",
      "controller_address": "<controller_lower>",
      "did": "<did>",
      "adapter": "<adapter_name>",
      "provisioner_version": "1.0.0",
      "provisioned_at": <unix_ts>,
      "provisioning_domain": "DCROPE-ENTITY-PROV",
      "auth_mode": "sso" | "wallet",
      "sso_sub": "<jwt.sub if Mode A>",
      "adapter_context_hash": "<hex(keccak256(canonical_json(adapter_payload)))>"
    }
  }]
}
```

`EntityGenesis` is a new interaction type; existing interaction types (`DataboxRegistered`, `ProjectSubmitted`, `PrivatePoolTreasuryEstablished`, ...) remain valid for their existing flows.

The `adapter_context_hash` commits to the full adapter payload without publishing it. This lets the adapter carry arbitrary platform-specific data (Tanastok asset ID, DCSwap pool address, NaturaProof claim ID) without bloating the on-chain payload.

### 7.6 Public card

Anchor a companion `EcosystemEntityRegistered` knot on the shared `EDC_REGISTRY_WALLET` (`0x...ec01`) so `dcscan.io/ecosystem` auto-lists:

```json
{
  "jsonrpc": "2.0", "id": 3,
  "method": "rope_appendToLedger",
  "params": ["0x000000000000000000000000000000000000ec01", {
    "interaction_type": "EcosystemEntityRegistered",
    "description": "<canonical_name>",
    "metadata": {
      "entity_id": "<entity_wallet>",
      "entity_kind": "<kind>",
      "canonical_name": "<canonical_name>",
      "display_name": "<raw_name_from_caller>",
      "archetype": "<canonical archetype from ecosystem_canonical::canonical_archetypes()>",
      "status": "live" | "development" | "sandbox",
      "region": "<optional geo>",
      "country": "<optional ISO 3166-1 alpha-2>",
      "logo_url": "<optional https url>",
      "description": "<short public description>",
      "tags": ["<optional tag>", ...],
      "stakeholder_url": "<optional partner platform URL>",
      "genesis_knot": "<hash of the EntityGenesis knot from §7.5>",
      "adapter": "<adapter_name>",
      "visibility": "public" | "private_visible" | "private_hidden",
      "controller_address": "<controller_lower>",
      "did": "<did>"
    }
  }]
}
```

`EcosystemEntityRegistered` is a **strict superset** of the existing `EcosystemProjectRegistered` schema used by the EDC. Existing consumers reading `EcosystemProjectRegistered` keep working; new consumers filter on `entity_kind` to get the right subset. The two interaction types coexist during the transition; the EDC keeps emitting `EcosystemProjectRegistered` for backward compat with any external tooling that pinned to the old name.

`visibility` respects the §30 canonical precedence: an entity that the operator has marked `private_hidden` in `ecosystem_canonical::canonical_entries()` (post-hoc, after auto-provisioning) will be filtered by the loader regardless of what's on-chain. The card's `visibility` is a preference; canonical is authority.

### 7.7 Overlay entry (fast-path listing via shared JSONL with `rope-ecosystem-discovery.service`)

After the on-chain sequence (§7.5 + §7.6) succeeds, the provisioner **appends an overlay entry** to the JSONL file **already owned by `rope-ecosystem-discovery.service`** at `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` (path fixed by the deployed systemd unit; see §7.8 for the file-ownership contract).

**Why write to a file the provisioner does not own.** The on-chain `EcosystemEntityRegistered` card from §7.6 is authoritative and will be picked up by `rope-explorer` on its next `refresh_ecosystem_directory_cache` tick. But that refresh may lag by up to `run_interval_secs` (default 900 s = 15 min per `/etc/rope-ecosystem-discovery.toml`) because the on-chain scanner only re-reads dcscan labels on that cadence. Writing the overlay row **inline in the provisioning response path** shortens the "entity provisioned → visible on dcscan.io/ecosystem" latency from ~15 min to under 30 s (one dc-explorer cache TTL). It is a belt-and-braces performance optimisation, not a source of truth.

**File is shared, not owned.** The provisioner does NOT stand up its own JSONL writer, its own systemd unit, or its own overlay path. It appends to the exact same file that `rope-ecosystem-discovery.service` writes on its scan cadence. This means:

1. **Single loader consumer** - `rope-explorer::ecosystem_overlay.rs` reads one path via `ECOSYSTEM_OVERLAY_PATH`. The loader does not care which producer wrote a given row.
2. **No coordination needed at read time** - dedup by lowercase `id` (first-write wins per pass) already handles the case where the on-chain scanner and the provisioner both emit a row for the same entity.
3. **Disaster recovery via re-scan** - if the overlay file is corrupted, deleted, or lost, `rope-ecosystem-discovery.service` regenerates it from scratch on its next scan pass (~100 ms per §31.9 deploy record). No provisioner state is on the critical path.
4. **Systemd write access already granted** - the `rope-ecosystem-discovery.service` unit declares `ReadWritePaths=/var/lib/rope-ecosystem-discovery`. The `rope-entity-provision` binary (or the `rope-explorer` HTTP handler when it emits) must be deployed under a systemd unit that either shares the same `User=ubuntu` posture OR carries the same `ReadWritePaths` grant. See §7.8 for the ownership contract.

**Schema.** The overlay entry uses `discovered_by: "entity_provisioner"` (a **new value** alongside the existing `handover-scanner`, `onchain-scanner`, `partner-api-scanner`, `manual` values in `ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md`). Landing this value requires extending the `discovered_by` enum in the overlay spec (v1.1 of that spec) and in `rope-ecosystem-discovery::entry::DiscoveredBy` (via a new variant `EntityProvisioner`). Both changes are additive; existing scanners keep emitting their existing values.

**Precedence.** The loader treats provisioner-written rows identically to scanner-written rows: overlay is the lowest-precedence source. Canonical entries (`ecosystem_canonical::canonical_entries()`) and EDC-registered projects both win on any collision. The `visibility` field of a provisioner-written row respects the §30 canonical precedence exactly like §7.6.

**Failure is non-fatal.** If the overlay-file append fails (disk full, permission denied, transient FS error), the provisioner logs a WARN and returns success to the caller anyway - the on-chain card from §7.6 is authoritative, so the entity will still appear on `/ecosystem` on the next `rope-ecosystem-discovery` scan tick. The overlay append is a fast-path, not a hard dependency.

### 7.8 Coordination contract with `rope-ecosystem-discovery.service`

This section formalises the operational contract between the provisioner (this spec) and the discovery daemon (per handover §31 + `docs/ROPE_ECOSYSTEM_DISCOVERY_RUNBOOK.md`). It is a **binding** contract - both sides must agree on it for the fast-path in §7.7 to work correctly.

#### 7.8.1 Deployment pre-requisite

`rope-ecosystem-discovery.service` MUST be deployed and healthy **before** the provisioner attempts any overlay append. The provisioner's startup check:

```rust
if !Path::new(&self.overlay_path).parent().unwrap().exists() {
    warn!("overlay parent dir missing; discovery service not deployed; fast-path disabled");
    self.fast_path_enabled = false;
}
```

If the parent dir does not exist, the provisioner disables the fast-path entirely (all §7.7 appends become no-ops). The on-chain sequence (§7.5 + §7.6) still runs and is still authoritative; the caller just waits up to `run_interval_secs` for the entity to appear on `/ecosystem` after the next canonical loader tick. This is a graceful degradation: the provisioner never hard-fails because of a missing discovery daemon.

Rollout order for a new environment (BLUE, GREEN, or a fresh DO node):

1. Deploy `rope-ecosystem-discovery.service` per `docs/ROPE_ECOSYSTEM_DISCOVERY_RUNBOOK.md` §"Fresh install on rope-vps".
2. Verify `systemctl is-active rope-ecosystem-discovery.service` = `active`.
3. Verify `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` exists (empty file is fine after a `--once` run with zero scanner hits).
4. Deploy the `rope-entity-provision` binary + systemd unit (Phase 2 of §14).
5. Verify the provisioner logs `fast-path enabled path=/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` on startup.

#### 7.8.2 File ownership + permissions

| Aspect | Value | Enforced by |
|---|---|---|
| File path | `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` | `deploy/rope-ecosystem-discovery.example.toml` `output_path` |
| Owner | `ubuntu:ubuntu` | `mkdir -p ... && chown ubuntu:ubuntu` at deploy time |
| Mode | `0644` (rw-r--r--) | `install(1)` default via systemd unit |
| Directory mode | `0755` (rwxr-xr-x) | `mkdir` default |
| Systemd `ReadWritePaths` (discovery) | `/var/lib/rope-ecosystem-discovery` | `deploy/rope-ecosystem-discovery.service` |
| Systemd `ReadWritePaths` (provisioner) | MUST include `/var/lib/rope-ecosystem-discovery` | `deploy/rope-entity-provision.service` (to be written for Phase 2) |
| Provisioner runs as | `ubuntu:ubuntu` (same as discovery) | `deploy/rope-entity-provision.service` |

The provisioner MUST run under the same `User=ubuntu, Group=ubuntu` as `rope-ecosystem-discovery.service` so that appends produce files with matching ownership. Running under a different UID would create files that the discovery daemon can read but not rewrite, breaking the file-rotation path (§7.8.4).

#### 7.8.3 Concurrent-write safety

Both the discovery daemon (on its `run_interval_secs` cadence, default 900 s) and the provisioner (on every successful `provision()` call) write to the same file. Two writer processes = potential race. The contract:

1. **Discovery daemon writes are atomic-full-rewrite via tmp + fsync + rename.** The daemon builds the complete overlay in a tmpfile, `fsync`s it, then `rename(2)`s over the live path. This is atomic per POSIX and any concurrent reader gets either the old or the new file, never a half-written one.
2. **Provisioner writes are append-only via `O_APPEND`.** Each provisioner append is one `write(2)` of `<serialised-json>\n` bytes. `O_APPEND` guarantees the write is atomic against concurrent appends from other provisioner instances (kernel serialises `O_APPEND` writes < `PIPE_BUF` = 4096 bytes on Linux, which comfortably fits any overlay entry).
3. **The race window is discovery-daemon-rewrite vs provisioner-append.** If the daemon `rename`s in between the provisioner's `open(2, O_APPEND)` and its `write(2)`, the provisioner's append lands in a file that the kernel has already unlinked (the old inode) - it succeeds but is invisible. Loss window ≤ 100 ms per scan cadence. Consequence: 1 in ~9000 provisioner writes may be lost this way (100 ms / 900,000 ms). This is acceptable because the on-chain card (§7.6) is authoritative; the next discovery scan will rediscover the entity via the on-chain `EcosystemEntityRegistered` knot.
4. **No file locking.** Adding `flock(2)` or advisory locks would create a hard dependency between the two services (daemon blocks on lock during rewrite → provisioner blocks → provisioning request stalls → cascading latency). The 100 ms loss window is a better trade than that coupling.

#### 7.8.4 Loader consumption from `rope-explorer`

`rope-explorer` reads the JSONL file via `ecosystem_overlay::load_overlay_cards_from(path)` (see `crates/rope-explorer/src/ecosystem_overlay.rs`). The loader:

1. Opens the file with `O_RDONLY`. Never mutates. Never holds an fd across ticks.
2. Reads line-by-line, applies `dedup by lowercase id` (first-write wins per file).
3. Refreshes on every `refresh_ecosystem_directory_cache` tick (default 60 s cache TTL).

Because the daemon rewrites atomically and the provisioner appends line-by-line, the loader always sees a syntactically valid JSONL file (worst case: it may miss a provisioner-appended row that lost the race in §7.8.3, but that row will be present in the next daemon rewrite because the on-chain scanner will discover the entity via §7.6).

The loader must be enabled via `ECOSYSTEM_OVERLAY_PATH` env var in `/opt/datachain-rope/code/deploy/.env`. Per §31.9.7, this env var is **not yet set on production** (waiting for the 24-hour soak of `rope-ecosystem-discovery.service` to complete). Until it is set, the entire overlay pathway (both scanner-written and provisioner-written rows) is dark; the on-chain card in §7.6 remains the sole source of truth. This is a **known operational gap**, not a spec defect.

#### 7.8.5 Disaster recovery

| Failure mode | Recovery |
|---|---|
| Overlay file deleted / truncated / corrupted | `rope-ecosystem-discovery.service` regenerates the full file on its next scan tick (~100 ms + `run_interval_secs`). Provisioner-written rows lost since the last scan are rediscovered from on-chain `EcosystemEntityRegistered` knots. Zero on-chain data loss. |
| `rope-ecosystem-discovery.service` down for >`run_interval_secs` | Provisioner keeps appending its own rows (fast-path stays live). When the daemon comes back up, its first scan rewrites the file, potentially discarding provisioner-appended rows that the daemon didn't re-emit (rare: only happens if the on-chain scanner filters out the entity for some reason). Recovery: manual overlay-file inspection + `journalctl -u rope-ecosystem-discovery.service` to understand the discrepancy. |
| `rope-explorer` env var unset (§31.9.7 status) | Entire overlay is dark. Both scanner and provisioner writes go to the file but nobody reads them. Recovery: set `ECOSYSTEM_OVERLAY_PATH` and restart `dc-explorer.service`. |
| Both services write with different `discovered_by` values for the same entity | Dedup by lowercase `id` first-write wins. Whichever process appended first survives; the other is silently discarded. Correctness preserved. |

#### 7.8.6 Observability contract

Both the provisioner and the discovery daemon MUST emit `tracing::info!` lines that a shared log-aggregation query can join:

- Provisioner: `info!("overlay fast-path append", entity_id = %id, discovered_by = "entity_provisioner", target_path = %overlay_path)`
- Discovery daemon (already implemented per §31.9.2): `INFO discovery pass complete considered=N emitted=M rejected=K elapsed_ms=T` + `INFO wrote overlay path=... entries=M`

An operator can then answer "did this entity land in the overlay via the fast-path or the scanner?" by grepping journal for the entity's `id`:

```bash
sudo journalctl -u rope-entity-provision.service -u rope-ecosystem-discovery.service \
    --since '1 hour ago' | grep -F "$ENTITY_ID"
```

Expected output for a fast-path win: one provisioner-side `overlay fast-path append` line, followed 15 min later by a discovery-daemon `discovery pass complete emitted=M` line (with M unchanged relative to the previous pass, because the entity was already there). Expected output for a scanner-only entity: no provisioner line, just the daemon lines. Both patterns are healthy.

#### 7.8.7 Schema evolution contract

The overlay JSONL schema is versioned via the top-level `ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` document. Adding a new `discovered_by` value (like `entity_provisioner`, per §7.7) requires:

1. Bump `ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` to v1.1 (this spec's v1.1 bump implies that spec must land first).
2. Add the variant to `rope-ecosystem-discovery::entry::DiscoveredBy` enum + the `is_valid_discovered_by()` check in the loader.
3. Add the variant to `rope-entity-provision::overlay::DiscoveredBy` enum.
4. Deploy the discovery daemon rebuild + the provisioner rebuild in either order (they don't need to be simultaneous; the loader treats unknown values as validation failure and drops the row - so during a partial rollout, the older loader will drop the new-value rows, which is safe because on-chain cards are authoritative).

Removing or renaming a value is a breaking change and requires a v2 spec bump.

#### 7.8.8 Non-goals of this coordination contract

- **The contract does NOT make the provisioner a scanner.** The provisioner writes exactly one entity per `provision()` call. It does not iterate, discover, or synthesise entities. All discovery duty stays with `rope-ecosystem-discovery.service`.
- **The contract does NOT couple deployment cadences.** The two services can be rebuilt, restarted, and rolled back independently. Neither has a hard runtime dependency on the other beyond the shared file path.
- **The contract does NOT gate the on-chain path.** The `EcosystemEntityRegistered` knot in §7.6 is written regardless of overlay-append outcome. On-chain state is the source of truth; the overlay is an eventually-consistent cache.

---

## 8. Platform adapter interface

```rust
#[async_trait]
pub trait EntityAdapter: Send + Sync {
    /// Adapter's canonical name, e.g. "tanastok", "dcswap", "naturaproof".
    fn name(&self) -> &'static str;

    /// Which kinds this adapter accepts (typically 1-2 out of the 7).
    fn supported_kinds(&self) -> &'static [&'static str];

    /// Validate the caller-supplied adapter_payload without side effects.
    /// Called BEFORE any on-chain work. Failures abort the whole provisioning.
    async fn preflight(&self, ctx: &ProvisionContext) -> Result<PreflightReport, AdapterError>;

    /// Commit the platform-specific state. Called AFTER on-chain steps 1-3
    /// have succeeded. Failures cause the caller to attempt on-chain
    /// rollback per §12.
    async fn commit(&self, ctx: &ProvisionContext) -> Result<AdapterOutput, AdapterError>;

    /// Compensating action for §12 rollback. Called if commit succeeded but
    /// a later step failed. Adapters that can't rollback (e.g. an on-chain
    /// DCNFT that's been minted) return AdapterError::Irreversible - the
    /// service surfaces this to the caller as a "manual cleanup required"
    /// warning attached to the response.
    async fn rollback(&self, ctx: &ProvisionContext, out: &AdapterOutput)
        -> Result<(), AdapterError>;
}
```

Each adapter lives in its own module under `crates/rope-entity-provision/src/adapters/`. Adapters are compiled into the crate (not dynamically loaded) so they can be reviewed as part of a single security surface. Adding a new adapter is a PR that adds one file.

### 8.1 Builtin adapters (ship in v1)

| Adapter | Kinds | Behaviour |
|---|---|---|
| `builtin/generic.rs` | `project`, `identity`, `cord` | No external platform side-effect. Just runs the on-chain sequence. Used when the operator wants an entity registered on Rope but there's no partner platform to notify. |
| `builtin/databox.rs` | `databox`, `node` | Wraps existing `crates/rope-explorer/src/databox_registry.rs::register_databox`. Preflight = existing validation. Commit = existing JSONL append + heartbeat setup. Rollback = deregister from JSONL. |
| `builtin/edc_project.rs` | `project` | Wraps existing `crates/rope-edc/src/registry.rs::anchor_public_card` + `provision.rs`. Handles the nine-step EDC wizard payload. |

### 8.2 Partner adapters (ship in v1)

| Adapter | Kinds | Preflight | Commit | Rollback |
|---|---|---|---|---|
| `tanastok/asset.rs` | `asset`, `contract` | Validate asset payload; check `tanastok.io/api/v1/tokenized-assets?contract=` returns 404 (not yet registered); reserve slug via `POST tanastok.io/api/v1/assets/reserve` with the SSO JWT. | `POST tanastok.io/api/v1/assets/register` with the reserved slug + DCNFT + ERC-3643 addresses. Deploy the contracts here OR expect them pre-deployed (adapter accepts both modes via `adapter_payload.mode = "deploy" | "attach"`). | `POST tanastok.io/api/v1/assets/deregister`; note that DCNFT tokens can't be un-minted (returns `Irreversible` if we minted in commit; can rollback if `mode: "attach"`). |
| `dcswap/pool.rs` | `contract`, `asset` | Validate token addresses; check `DCSwapFactory.getPair` returns zero. | `DCSwapFactory.createPair` + optional `Router.addLiquidity`. Publish pool card. | `Router.removeLiquidity` if we added, then note pair can't be undone. |
| `naturaproof/verify.rs` | `asset` (biodiversity claim) | Validate GPS coordinates, species list, verifier ID. | `POST naturaproof.com/api/v1/claims/register`. | `POST naturaproof.com/api/v1/claims/withdraw`. |
| `syndicated/investment.rs` | `asset` (investment vehicle), `identity` (fund manager) | Validate KYC/KYB against issuer's ONCHAINID. | `POST syndicated.ltd/api/v1/vehicles/register`. | `POST syndicated.ltd/api/v1/vehicles/close`. |
| `edc/node.rs` | `node` | Wraps existing `rope-edc/src/provision.rs::provision_node`. Preflight validates `NodePlan`. | Calls `rope-deployer` with `ProvisionRequest`. Anchors `NodeProvisioned` knot. | Calls `rope-deployer` teardown. |

Each adapter's commit is idempotent - re-running the same `provision` call with the same idempotency key finds the existing platform-side entity and returns its identifier instead of creating a duplicate.

---

## 9. Client interfaces

### 9.1 CLI: `rope-entity-provision`

Ships as a binary in the `rope-entity-provision` crate. Distributed via `deploy/node-package/scripts/`.

```
USAGE:
  rope-entity-provision <SUBCOMMAND> [OPTIONS]

SUBCOMMANDS:
  provision    Provision a new entity (or return existing if idempotent match)
  update       Update an existing entity's card (only fields marked mutable in §12.3)
  deprecate    Mark an entity as deprecated (does not delete; anchors EcosystemEntityDeregistered)
  list         List entities you control (via SSO JWT or wallet address)
  show         Show the full entity record

OPTIONS (provision):
  --kind <k>                Required. One of: project, databox, node, asset, contract, identity, cord
  --name <n>                Required. Display name; will be canonicalised per §7.2
  --adapter <a>             Optional. Adapter name (defaults to builtin for the kind)
  --controller-address <a>  Optional. BYO wallet (if omitted, deterministic per §7.1)
  --auth-mode <m>           Required. Either "sso" (uses Datachain ID JWT) or "wallet" (needs --private-key)
  --private-key <hex>       Required if --auth-mode wallet; NEVER logged
  --sso-token <jwt>         Optional in sso mode; reads from ~/.config/rope/id-token.json or DATACHAIN_ID_TOKEN env
  --region <r>              Optional
  --country <c>             Optional ISO 3166-1 alpha-2
  --description <d>         Optional short description
  --logo-url <url>          Optional HTTPS URL
  --tag <t>                 Repeatable
  --stakeholder-url <url>   Optional partner platform URL
  --adapter-payload-file <p> Optional JSON file for adapter-specific fields
  --visibility <v>          Optional: public (default) | private_visible | private_hidden
  --endpoint <url>          Optional API endpoint (default https://dcscan.io/v1/ecosystem/provision)
  --dry-run                 Validate + auth + preflight, but don't commit
  --json                    Emit machine-readable JSON result

EXAMPLES:
  # Databox operator, no Datawallet+, plain EVM wallet
  rope-entity-provision provision \
    --kind databox \
    --name my-fra1-databox-02 \
    --auth-mode wallet \
    --private-key 0x... \
    --region eu-west \
    --country FR \
    --stakeholder-url https://ops.example.com/databoxes/02

  # Tanastok issuer, SSO, deploying a new asset with adapter payload
  rope-entity-provision provision \
    --kind asset \
    --name "Kibali Gold Mine, Congo DRC" \
    --adapter tanastok \
    --auth-mode sso \
    --country CD \
    --adapter-payload-file kibali-payload.json

  # Idempotent re-run (safe, returns existing entity)
  rope-entity-provision provision \
    --kind databox --name my-fra1-databox-02 \
    --auth-mode wallet --private-key 0x...
    # -> returns existing entity_id, no new knots anchored
```

Exit codes:
- `0` on success (including idempotent hit)
- `1` on validation failure (bad --kind, malformed name, adapter preflight failure)
- `2` on auth failure
- `3` on on-chain failure (rope-node unreachable, RPC error)
- `4` on adapter commit failure (attempted rollback, may or may not have succeeded)
- `5` on partial-commit-with-irreversible-adapter (manual cleanup needed; details in stderr)

### 9.2 HTTP: `POST /v1/ecosystem/provision`

Lives on `dcscan.io` (via `rope-explorer`). Same auth + payload shape as the CLI.

Request:

```
POST /v1/ecosystem/provision HTTP/1.1
Host: dcscan.io
Authorization: Bearer <jwt>      (Mode A)
Content-Type: application/json

{
  "kind": "asset",
  "name": "Kibali Gold Mine, Congo DRC",
  "adapter": "tanastok",
  "adapter_payload": { ... },
  "region": "africa",
  "country": "CD",
  "description": "Gold mine tokenized as ERC-3643 shares",
  "logo_url": "https://tanastok.io/uploads/kibali-hero.jpg",
  "tags": ["mining", "gold"],
  "stakeholder_url": "https://tanastok.io/assets/kibali-gold-mine",
  "visibility": "public",
  "idempotency_key": "tanastok:kibali-2026-08"

  // Mode B additions (omit for Mode A):
  //   "controller_address": "0x...",
  //   "timestamp": 1786572000,
  //   "signature": "0x...65 bytes hex..."
}
```

Response 201 Created (or 200 OK on idempotent hit):

```json
{
  "success": true,
  "idempotent_hit": false,
  "entity": {
    "entity_id": "0x...",              // the entity wallet
    "entity_kind": "asset",
    "canonical_name": "kibali gold mine, congo drc",
    "display_name": "Kibali Gold Mine, Congo DRC",
    "wallet": "0x...",
    "did": "did:web:datawallet.plus:bf92baf8-...",
    "controller_address": "0x...",
    "auth_mode": "sso",
    "genesis_knot": "0x<hash>",
    "card_knot": "0x<hash>",
    "adapter": "tanastok",
    "adapter_output": {
      "tanastok_asset_id": "featured-kibali-gold-mine",
      "dcnft_address": "0x...",
      "erc3643_address": "0x..."
    }
  },
  "warnings": []
}
```

Error responses use the same envelope shape as other `dcscan.io` endpoints (`{"success": false, "error": {"code": "...", "message": "..."}, "warnings": []}`), with these error codes: `validation_failed`, `auth_missing`, `auth_invalid`, `auth_expired`, `signature_stale`, `signature_mismatch`, `unknown_kind`, `unknown_adapter`, `preflight_failed`, `rope_node_unreachable`, `rope_node_error`, `adapter_commit_failed`, `adapter_commit_failed_and_rollback_failed_irreversible` (the manual-cleanup case), `duplicate_idempotency_key_different_body`.

### 9.3 EDC wizard delegation (unchanged from operator POV)

The EDC wizard at `console.datachain.network` doesn't get a UX change. Internally, its `submit_wizard` handler stops calling `crates/rope-edc/src/registry.rs::ensure_ledger + anchor_public_card` directly and instead delegates to `rope_entity_provision::provision_entity(ProvisionContext { kind: "project", adapter: "edc_project", ...wizard_fields })`. The wizard payload maps 1:1 onto `ProvisionContext.adapter_payload`. Behaviour is identical from the operator's POV; the delegation gives us one code path for on-chain state.

---

## 10. Full provisioning workflow (state machine)

```
                       ┌─────────────────────┐
                       │   CALLER REQUEST    │
                       │ (CLI or HTTP body)  │
                       └─────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  1. PARSE + VALIDATE       │
                    │  - shape check             │
                    │  - kind ∈ §5               │
                    │  - name canonicalise §7.2  │
                    │  - archetype ∈ allowlist   │
                    └────────────────────────────┘
                                 │
                     ┌───────────┴───────────┐
                     ▼                       ▼
              ┌──────────────┐       ┌──────────────┐
              │  MODE A: SSO │       │  MODE B: SIG │
              │  verify JWT  │       │  verify EIP191│
              │  extract     │       │  recover addr │
              │  did+wallets │       │  match owner  │
              └──────────────┘       └──────────────┘
                     │                       │
                     └───────────┬───────────┘
                                 ▼
                    ┌────────────────────────────┐
                    │  2. RESOLVE controller +   │
                    │     entity_wallet + DID    │
                    │     per §7.1 + §7.3        │
                    └────────────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  3. COMPUTE idempotency    │
                    │     hash(kind,             │
                    │          canonical_name,   │
                    │          controller,       │
                    │          idempotency_key)  │
                    └────────────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  4. LOOKUP existing entity │
                    │     if found: verify no    │
                    │     schema drift and       │
                    │     return existing (200)  │
                    └────────────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  5. ADAPTER preflight      │
                    │     - validate payload     │
                    │     - reserve external ids │
                    │     - dry-run external tx  │
                    │     (fail here = clean abort)│
                    └────────────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  6. rope_createPersonal    │
                    │     Ledger(entity_wallet,  │
                    │             {kind})        │
                    │  (idempotent per Canon 1.2)│
                    └────────────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  7. rope_appendToLedger    │
                    │     EntityGenesis knot on  │
                    │     entity_wallet's string │
                    └────────────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  8. rope_appendToLedger    │
                    │     EcosystemEntityReg     │
                    │     card on registry wallet│
                    └────────────────────────────┘
                                 │
                                 ▼
                    ┌────────────────────────────┐
                    │  9. ADAPTER commit         │
                    │     (platform-specific)    │
                    └────────────────────────────┘
                                 │
                     ┌───────────┴───────────┐
                     ▼                       ▼
             ┌──────────────┐        ┌──────────────┐
             │  SUCCESS     │        │  ADAPTER FAIL│
             │  10a. write  │        │  10b. attempt│
             │  overlay row │        │  adapter     │
             │  10a. return │        │  rollback,   │
             │  201 Created │        │  emit        │
             │              │        │  ProvRollback│
             │              │        │  knot,       │
             │              │        │  return 4xx  │
             │              │        │  or 5xx      │
             └──────────────┘        └──────────────┘
```

Steps 6-9 are the "commit phase". Steps 1-5 have no on-chain effect and can be re-run freely. Step 10 is the "post-commit" phase (overlay write) - if it fails, the entity is still fully provisioned; the overlay just doesn't get the fast-path row and dcscan will discover it on the next refresh anyway. Step 10 is not required for correctness.

---

## 11. Idempotency + resumability + rollback

### 11.1 Idempotency key

Server computes:
```
idem_key = hex(keccak256(
    "DCROPE-ENTITY-PROV-IDEM/v1\0"
    || kind
    || 0x00
    || canonical_name
    || 0x00
    || controller_address_lower
    || 0x00
    || (caller_supplied_idempotency_key OR "")
))
```

Stored in a small on-disk keyed lookup (`/opt/datachain-rope/entity-provisioning-idem.jsonl`, atomic tmp+rename per write, same pattern as `databox_registry.rs::save_databoxes_local`). Every entry records `{idem_key, entity_id, provisioned_at, request_hash}`.

Re-runs:
- Same `idem_key` + same `request_hash` (byte-identical request except timestamp/signature) → return the existing entity, HTTP 200 with `"idempotent_hit": true`.
- Same `idem_key` + different `request_hash` → HTTP 409 with error `duplicate_idempotency_key_different_body`. Caller has to either use a different idempotency key or update the existing entity via the `update` subcommand.

### 11.2 Resumability

If step 6 (`rope_createPersonalLedger`) succeeded but step 7 (`EntityGenesis` knot) failed:
- Idem-lookup finds no entry (only step 4 writes it).
- Next attempt re-runs step 6, which the node returns `2001 Ledger already exists` (idempotent).
- Steps 7-9 run as normal.
- Result is correct.

If step 8 (public card) succeeded but step 9 (adapter commit) failed:
- Idem-lookup entry exists (persisted between steps 8 and 9).
- Next attempt short-circuits with the previous partial state, then retries step 9 alone.
- If step 9 still fails, we log and return the same error.

If steps 7 or 8 fail after step 6 succeeded and we crash before writing idem, we get a lingering unused ledger. This is harmless (Quipu strings are cheap; an unused wallet's string just has 0 knots and 0 tombstones) but ugly. A janitor sweep (out of scope for v1) can garbage-collect strings that have no knots and no idem entry after 24h.

### 11.3 Rollback on adapter failure

If step 9 fails and the adapter returns a reversible error:
- Call `adapter.rollback(...)`. If it succeeds:
  - Anchor `EntityGenesisRolledBack` knot on the entity's string (records the reason).
  - Anchor `EcosystemEntityDeregistered` on the registry wallet (removes it from dcscan listing).
  - Do NOT untie the genesis knot (Quipu principle: history is preserved even when the entity is rolled back).
  - Return HTTP 4xx.
- If adapter rollback fails OR returns `Irreversible`:
  - Anchor `EntityProvisioningPartial` knot on the entity's string explaining what committed and what didn't.
  - Leave the ecosystem card in place (with a `status: "partial"` badge so operators can see something needs cleanup).
  - Return HTTP 5xx with detailed error including manual-cleanup instructions.
  - Trigger a CERBER page (via `cerber-edge-ingest` on the existing wire) for operator attention.

The "rollback the adapter but keep the Quipu record" posture matches Quipu Canon's tombstone philosophy: erasure requires an explicit `rope_untieKnot` call, and even then only the payload is destroyed while the position is preserved.

---

## 12. Failure modes + graceful degradation

| Failure | Detection | Response |
|---|---|---|
| Datachain ID SSO unavailable (Mode A only) | JWT signature verification fails against JWKS (JWKS unreachable, key rotated) | HTTP 503 with `auth_provider_unavailable`. Caller can retry OR switch to Mode B for this session. |
| Datawallet+ SSO issued a JWT with no `wallets[]` | Mode A resolver returns empty list | Fall back to synthetic `did:rope:entity:<hash>` and log warn. Operator can attach a wallet later via `update`. |
| Rope-node reachable but 24h wedge-cycle in progress | `rope_createPersonalLedger` times out | HTTP 503 with `rope_node_degraded`, retryable. SWR-cached fleet-status appended so caller can see the outage window. |
| Rope-node accepts create but rejects append (V11 gate) | `rope_appendToLedger` returns `-32401 Method denied on public listener` | HTTP 500 with `rope_node_v11_denial`. Indicates a config problem (provisioner is not on loopback and doesn't have `X-Rope-Internal-Token`). Operator alert. |
| Adapter preflight fails (Tanastok reserves rejects the slug) | Adapter returns typed error before commit | HTTP 4xx with adapter's error message. No on-chain state changed. |
| Adapter commit half-succeeded (e.g. DCNFT minted but ERC-3643 deploy failed) | Adapter's commit returns partial-success | Same as §11.3 "Irreversible" path. Card status flips to `partial`. Manual cleanup required. |
| Overlay write fails (disk full, permission error) | Post-commit step 10 error | Log warn, return success anyway (entity IS registered). Overlay will be rebuilt on next `rope-ecosystem-discovery` refresh. |
| Idempotency store corruption | JSONL parse failure on load | Rebuild from Rope by querying `rope_getStringWithKnots` on the registry wallet + filtering by `EcosystemEntityRegistered` interactions. Same pattern as `databox_registry::rebuild_databoxes_from_rope`. |

Every failure mode is enumerable, every response is honest ("we did register on Rope, but the platform commit failed" is a distinct outcome from "we couldn't register at all"), and every recoverable failure returns a 5xx that clients can retry safely.

---

## 13. Security model

| Threat | Mitigation |
|---|---|
| Signature replay against a different domain | `DCROPE-ENTITY-PROV` domain tag in every EIP-191 message. |
| Signature replay within the same domain (different action) | `{action}` in message; also `{payload_hash}` commits to full request. |
| Signature replay in time | ±300s freshness window. Same as `DATABOX_AUTH_WINDOW_SECS`. |
| Signature replay in space (from a partner's data) | Server-side nonce store (in-memory `HashSet` keyed by signature hash, 10-minute TTL). Second submission of same sig within window rejected. |
| Attacker registers an entity in someone else's name | `controller_address` in message must match recovered EIP-191 signer. Mode A: JWT's `sub` binds the request. |
| Attacker uses a leaked Datachain ID JWT | JWT has 24h TTL. Compromise blast-radius bounded. Operator can revoke by rotating password on `id.datachain.network`. |
| Attacker submits garbage adapter payload to cause DoS | Preflight validates and returns 4xx fast. Rate-limit on `/v1/ecosystem/provision` (10 req/min per source IP, 60/min per SSO sub). |
| Attacker uses provisioner to spam on-chain state | Rate limit + Datachain ID `wallets[]` gate: only wallets already in the operator's Datawallet+ account can be `controller_address`. |
| Attacker registers a valid-looking entity with hostile metadata (phishing description, malicious logo URL) | `logo_url` must resolve to https + allowlist of hosts (see `ecosystem_canonical::canonical_entries` logo policy). `description` HTML-escaped by the frontend. `stakeholder_url` shown behind an interstitial for unknown hosts. Same posture as `ecosystem_canonical` today. |
| Compromised deployer wallet used as controller | Reject `0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195` explicitly, mirroring the `deploy-migration-minter.mjs` refusal (Section §22 of the deployer rotation handover). |
| SSRF via adapter fetching hostile URL | Every adapter's `preflight` and `commit` MUST use the shared `rope-security::http_client` which enforces the platform-specific allowlist (Tanastok → `tanastok.io` only, DCSwap → `dcswap.net`, etc.). |
| Man-in-the-middle on ingestion endpoint | HTTPS with LE certs (existing). |
| Log leakage of private keys (Mode B) | CLI zeroises the `--private-key` in memory as soon as sig is produced. HTTP surface never accepts a private key; callers must sign client-side. |

---

## 14. Rollout plan

Phased, additive, no destructive migration of existing state.

### Phase 0 - Freeze this spec

- Land this doc (`docs/SPEC_ECOSYSTEM_ENTITY_PROVISIONING_V1.md`).
- Get sign-off that the wire shape (`DCROPE-ENTITY-PROV` domain, `EcosystemEntityRegistered` schema, `EntityGenesis` interaction, adapter trait) is stable.

### Phase 1 - Implement the crate (`crates/rope-entity-provision`)

- Core module: `lib.rs`, `provision.rs`, `types.rs`, `signing.rs`, `idempotency.rs`, `errors.rs`.
- Auth modules: `auth/sso.rs` (JWT verify against `id.datachain.network` JWKS), `auth/wallet_sig.rs` (reuses `walletsig` helpers).
- Adapter trait: `adapter.rs`.
- Builtin adapters: `adapters/{generic,databox,edc_project}.rs`.
- CLI binary: `bin/rope_entity_provision.rs`.
- Unit tests: ≥50 covering canonicalisation, signing, idempotency, adapter rollback matrix.
- No wire-level integration yet.

### Phase 2 - Wire the HTTP surface on `dcscan.io`

**Hard pre-requisites** (block Phase 2 launch until all three are green):

1. **`rope-ecosystem-discovery.service` deployed + healthy on the target rope-vps.** Per handover §31.9 (Phase A complete 2026-08-13T~19:20Z on production BLUE). Verify: `ssh rope-vps 'systemctl is-active rope-ecosystem-discovery.service'` returns `active`, and `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` exists (empty is fine). Without this, the provisioner's fast-path (§7.7) degrades silently to on-chain-only, adding ~15 min latency between provisioning and `/ecosystem` visibility - functional but a poor operator experience.

2. **Overlay reader activated on `dc-explorer.service`.** `ECOSYSTEM_OVERLAY_PATH=/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` must be present in `/opt/datachain-rope/code/deploy/.env` (Phase B of §31.9, deferred pending 24h soak). Without this env var, the entire overlay pathway is dark and the fast-path in §7.7 delivers zero user-visible benefit. Verify: `curl -sS https://dcscan.io/api/v1/ecosystem/directory | jq '[.projects[] | select(.source | startswith("overlay:"))] | length'` returns `>0`.

3. **Overlay schema v1.1 landed.** `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` bumped to v1.1 with `entity_provisioner` added to the `discovered_by` enum, and the `rope-ecosystem-discovery` binary rebuilt from the extended `DiscoveredBy` enum. Without this, the loader will drop every provisioner-authored row as a validation failure, silently disabling the fast-path even though the file writes succeed. Per §7.8.7, this is a coordinated schema bump: spec first, then discovery daemon, then provisioner.

**Wiring work:**

- Add `POST /v1/ecosystem/provision` route in `rope-explorer`.
- Route calls `rope_entity_provision::provision_entity`.
- Add rate-limit per `.cursor/rules/handover-*` posture.
- Feature-flag `ROPE_ENTITY_PROV_ENABLED=1` on `dc-explorer.service`. Default OFF. Deploy is a systemd drop-in.
- Add `/var/lib/rope-ecosystem-discovery` to `dc-explorer.service`'s `ReadWritePaths` (matches `rope-ecosystem-discovery.service`'s ownership of `ubuntu:ubuntu 0644`). Verify per §7.8.2.
- Provisioner CLI + service should run as the same `ubuntu:ubuntu` user as `rope-ecosystem-discovery.service` to preserve file ownership consistency for atomic-rewrite compatibility (§7.8.3).
- Manual QA against a single databox provisioned via Mode B, then re-tested via Mode A. Verify per §7.8.6 that both service journals emit the expected paired log lines (`overlay fast-path append` from the provisioner within seconds of provisioning; `discovery pass complete` from the discovery daemon on its next 15-min tick with unchanged emitted count).

### Phase 3 - Partner adapters

- `adapters/tanastok/asset.rs`: coordinate with the Tanastok agent to expose `/api/v1/assets/reserve` + `/api/v1/assets/register` endpoints that accept the SSO JWT.
- `adapters/dcswap/pool.rs`: coordinate with the DCSwap agent to expose `/v1/pools/register`.
- `adapters/naturaproof/verify.rs`, `adapters/syndicated/investment.rs`: same handshake with those agents.
- `adapters/edc/node.rs`: delegate the EDC wizard's `submit_wizard` to this crate.

Each adapter ships as a separate PR, feature-flagged per adapter (`ROPE_ENTITY_PROV_ADAPTER_TANASTOK=1`, etc.) so operators can enable them independently as partner readiness lands.

### Phase 4 - Deprecate the standalone databox script

- Publish `deploy/node-package/scripts/register-databox.sh` v2 that just calls `rope-entity-provision` under the hood. Preserves the same CLI flags so operators don't need to change their runbooks.
- Old server-side `POST /api/v1/databoxes/register` keeps working forever (Datachain compat promise).
- Once >90% of new registrations flow through `/v1/ecosystem/provision`, mark the old endpoint deprecated in dcscan API docs (still functional, just deprecated).

### Phase 5 - Retire duplicate registry wallets (optional, long-term)

- `DATABOX_LEDGER_WALLET` (`0x...d003`) and `EDC_REGISTRY_WALLET` (`0x...ec01`) each carry ecosystem cards today. Long-term consolidation would emit both `DataboxRegistered` (on `d003`, for legacy readers) AND `EcosystemEntityRegistered` (on `ec01`, for the unified reader) during Phase 2-3, then Phase 5 could stop emitting the `d003` copy.
- Not urgent. Both wallets are cheap; keeping both means zero risk to existing consumers.

Total engineering estimate: Phase 1 = 5 days, Phase 2 = 2 days, Phase 3 = 3 days per adapter × 5 adapters = 15 days (parallelisable), Phase 4 = 1 day, Phase 5 = deferred indefinitely. Realistic ship-window for Phase 1+2 (the useful minimum): **1 week**. Full partner-adapter set: **~4-6 weeks** including partner-side handshake work.

---

## 15. Compatibility matrix with existing systems

| Existing surface | Post-Phase-2 behaviour |
|---|---|
| `POST https://dcscan.io/api/v1/databoxes/register` | Unchanged. `databox_registry.rs` still handles it directly. Also emits an `EcosystemEntityRegistered` mirror card. |
| `POST https://console.datachain.network/v1/projects/submit` (EDC wizard) | Unchanged UX. Server delegates to `rope_entity_provision::provision_entity` under the hood. |
| `GET https://tanastok.io/api/v1/tokenized-assets` (manifest) | Unchanged. Provisioner adapter is a producer, not a consumer. `rope-node::entity_manifest.rs` keeps polling as today. |
| `GET https://dcscan.io/api/v1/ecosystem/directory` | Extended. Now merges: EDC cards (from `ec01` reading `EcosystemProjectRegistered` OR `EcosystemEntityRegistered`) + `ecosystem_canonical.rs` + overlay + `EcosystemEntityRegistered` on `ec01`. Precedence unchanged (EDC > canonical > overlay). |
| `rope_appendToLedger` | Unchanged. Provisioner is one more caller. |
| `rope_createPersonalLedger` | Unchanged. Optional second-parameter kind hint uses Canon v1.2 extension when the node supports it; graceful fallback otherwise. |
| Existing knot interaction types (`DataboxRegistered`, `ProjectSubmitted`, `EcosystemProjectRegistered`, ...) | All preserved. New types (`EntityGenesis`, `EcosystemEntityRegistered`, `EcosystemEntityDeregistered`, `EntityProvisioningPartial`, `EntityGenesisRolledBack`) are additive. |
| `dcscan.io/ecosystem` frontend | Extended to recognise the new `kind` values (`databox`, `node`, `identity`, `cord` badges added). Existing project cards render identically. |
| `dcscan.io/databoxes` frontend | Unchanged. Renders `DATABOX_LEDGER_WALLET` JSONL as today. |
| `dcscan.io/address/<addr>` | Unchanged renderer; picks up new `visibility` and `entity_kind` fields naturally through the existing card lookup. |
| **`rope-ecosystem-discovery.service` (deployed handover §31.9, active on `rope-vps` 2026-08-13T~19:20Z)** | **Coordinating peer, not an unchanged consumer.** The provisioner writes rows to the same `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` file that the daemon owns (see §7.7 + §7.8). The `OnchainScanner` still discovers `EcosystemEntityRegistered` cards on the next 15-min pass as a durable backfill, but the provisioner-authored row lets the entity appear on `dcscan.io/ecosystem` in seconds instead of minutes. The daemon needs one contract change (extend `DiscoveredBy` enum with `entity_provisioner`, per `ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` schema bump to v1.1) before Phase 2 launches, otherwise the loader validates the provisioner-authored rows out. |
| **`ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md`** | **Coordinated schema bump v1.0 -> v1.1.** Add `entity_provisioner` to the `discovered_by` enum. Add optional `source_platform` field (mirrors the `PlatformAdapter::platform_id()` value). No breaking change to existing consumers; older discovery daemons still write valid v1.0 rows. Loader in `rope-explorer` treats unknown `discovered_by` values as a warn-and-skip, so a stale loader deployed alongside a fresh provisioner degrades safely to on-chain-only. |
| **`/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` (shared JSONL file)** | **Multi-writer coordination.** Owned by `rope-ecosystem-discovery.service` (user `ubuntu:ubuntu`, mode `0644`); provisioner appends under the same UID via `write_overlay_atomic` (tmp+fsync+rename). Both writers dedup by lowercase `id` on merge; last writer wins on same-tick collisions, which is acceptable because both writers derive from the same on-chain state. `systemd` unit `rope-ecosystem-discovery.service` must be `active` before Phase 2 launches (verified via `systemctl is-active`). |
| `/opt/datachain-rope/code/deploy/.env` -> `ECOSYSTEM_OVERLAY_PATH` | Read-flip. Deferred pending the 24-hour soak of `rope-ecosystem-discovery.service` (per handover §31.9.4). Until this env var is set, the fast-path in §7.7 is dark and provisioner-authored rows sit on disk with no user-visible effect. Phase 2 hard-blocks on this flip. |

**Zero migration required for existing knot readers.** No knots are deleted. No wallets are re-derived. No existing consumers need to change. The provisioner is a new caller of existing endpoints + emits new knot types alongside the old ones. Rollback = disable the feature flag + old flows keep working.

**One coordinated schema change required for the discovery daemon:** `ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` must land v1.1 (adds `entity_provisioner` to `DiscoveredBy`), the `rope-ecosystem-discovery` binary must be rebuilt from the extended enum, and the rebuild must be deployed to `rope-vps` before the provisioner starts writing overlay rows. This is the only sequenced-dependency between the two services; everything else is independent.

---

## 16. Open questions (to resolve before Phase 1)

1. **Bulk provisioning.** Should there be a `POST /v1/ecosystem/provision/batch` that provisions N entities atomically? Useful for Tanastok importing 200 assets at once. Trade-off: complicates idempotency + rollback. Leaning towards NO for v1, YES for v2 with explicit two-phase commit semantics.
2. **DID verification.** How aggressive should DID document verification be? For `did:web:datawallet.plus:<uuid>` the JWT already proves ownership. For caller-supplied DIDs the document should be fetched and checked for a `serviceEndpoint` pointing back at the entity wallet. Recommend: skip in v1, add as a Phase 2 hardening.
3. **Rate-limit granularity.** 10 req/min per IP is a floor. Datachain Foundation operator provisioning 200 Tanastok assets in a batch shouldn't get rate-limited. Recommend: per-SSO-sub bump to 500/min for verified operators, 10/min for anonymous Mode B.
4. **Rollback observability.** Should we publish a CERBER-mesh signal on every rollback? Would page the operator whenever an adapter commit fails. Alternative: only page on `Irreversible`. Recommend the latter (reduce alert fatigue).
5. **Backward-compat for existing `EcosystemProjectRegistered` consumers.** The EDC and dcscan frontend both currently read that interaction type by name. Do we emit BOTH `EcosystemProjectRegistered` and `EcosystemEntityRegistered` for kind=`project` during Phase 2-3? Recommend YES for one release, then drop the old emission after all consumers upgrade.
6. **Partner adapter security review.** Each partner adapter is a piece of code that talks to a partner platform. Do we require an independent security review per adapter? Recommend YES (per adapter, at least the SSRF surface + auth token handling + rollback correctness).
7. **CLI distribution.** Include `rope-entity-provision` in `deploy/node-package/` alongside the existing register-databox script? Or ship as a standalone install (`cargo install` / GitHub release binaries)? Recommend both: node package for operators who already have the datachain-rope build, plus GitHub release binaries for partner integrators who don't want to compile.

---

## 17. Reference paths

| Where | What |
|---|---|
| `crates/rope-explorer/src/databox_registry.rs` | Existing databox flow, template for auth + persistence + rebuild-from-rope pattern |
| `crates/rope-edc/src/{types,registry,provision,grants}.rs` | Existing EDC flow, template for `Project` shape + wallet derivation + node provisioning |
| `crates/rope-idp/src/{routes,walletsig,identity}.rs` | Datachain ID SSO reference for JWT + wallet-sig verification |
| `crates/rope-node/src/rpc_server.rs` | `rope_createPersonalLedger` + `rope_appendToLedger` implementations |
| `crates/rope-core/src/personal_ledger.rs` | `StringKind` enum + ledger descriptor |
| `crates/rope-ecosystem-discovery/*` | Discovery daemon source. `OnchainScanner` (in `src/scanners/onchain.rs`) is the durable-backfill peer that discovers `EcosystemEntityRegistered` cards on every 15-min pass; the provisioner's post-commit step 10 co-writes to the same output file to shortcut the wait |
| `deploy/rope-ecosystem-discovery.service` | Systemd unit for the discovery daemon (deployed to `/etc/systemd/system/rope-ecosystem-discovery.service` on `rope-vps`, active + enabled since handover §31.9 landing 2026-08-13T~19:20Z). Runs as `ubuntu:ubuntu` under `ExecStart=/home/ubuntu/datachain-rope/target/release/rope-ecosystem-discovery --config /etc/rope-ecosystem-discovery.toml`. `ReadWritePaths=/var/lib/rope-ecosystem-discovery` is the shared coordination point |
| `deploy/rope-ecosystem-discovery.example.toml` | Config template for the discovery daemon (deployed to `/etc/rope-ecosystem-discovery.toml` on `rope-vps`). Documents the `output_path` default (`/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl`) that the provisioner MUST also target |
| `docs/ROPE_ECOSYSTEM_DISCOVERY_RUNBOOK.md` | Operator runbook for the discovery daemon: fresh install, health checks, one-command rollback, config knobs (`run_interval_secs`, `http_timeout_secs`, per-scanner enable flags), read-flip gate for `dc-explorer.service`. Provisioner deploy pre-flight (§7.8.1) references this |
| `docs/ECOSYSTEM_OVERLAY_JSONL_SPEC_V1.md` | Overlay contract that the provisioner's post-commit step 10 writes to. **Must land v1.1 (adds `entity_provisioner` to `discovered_by` enum + optional `source_platform` field) before Phase 2 launches, per compatibility matrix §15** |
| `/var/lib/rope-ecosystem-discovery/ecosystem-overlay.jsonl` | Shared JSONL file on `rope-vps` (mode `0644`, owner `ubuntu:ubuntu`). Written by both `rope-ecosystem-discovery.service` (every 15 min, full snapshot via `write_overlay_atomic`) and `rope-entity-provision` (per-provision append via same atomic tmp+fsync+rename primitive). Dedup by lowercase `id` at loader time |
| `.cursor/rules/handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc` §31.9 | Deployment record for `rope-ecosystem-discovery.service` on `rope-vps` - confirms daemon is `active` + `enabled`, overlay file materialised with 14 entries, and the 24h soak preceding the `ECOSYSTEM_OVERLAY_PATH` env-var flip on `dc-explorer.service` (which the provisioner's fast path also depends on) |
| `docs/SPEC_TANASTOK_ENTITY_INTEGRATION_V1.md` | Tanastok-specific integration reference for the `tanastok/asset.rs` adapter |
| `docs/ECOSYSTEM_DEPLOYMENT_CONSOLE_SPEC_V2.md` | EDC spec for the `edc_project.rs` and `edc/node.rs` adapters |
| `deploy/node-package/scripts/register-databox.sh` | Existing script that Phase 4 will replace |
| `.cursor/rules/handover-security-audit-2026-06-11.mdc` | V11 destructive-method gate context - relevant because the provisioner calls destructive `rope_*` methods on loopback |

---

*This spec is the source of truth for cross-platform entity provisioning as of 2026-08-13. Any change to the wire format (`DCROPE-ENTITY-PROV` domain, `EcosystemEntityRegistered` schema, adapter trait shape) requires a version bump and an explicit compatibility note here.*
