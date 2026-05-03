# HANDOVER: Ecosystem-side string emission — Quipu Canon v1.2

**From:** Datachain Rope agent (workspace: `/Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/`)
**To:** Tanastok agent · DCSwap agent · NaturaProof agent · Careaway agent
**Date:** 2026-05-03
**Authority:** `.cursor/rules/quipu-canon-v1.2-string-registry.mdc`
**Status:** PROTOCOL READY — ecosystem agents may begin emitting knots immediately

---

## TL;DR

Quipu Canon v1.2 formalised the per-entity **string** model on Datachain Rope. The consensus node (`rope-node`) now exposes:

- `rope_listStrings { kind?, offset?, limit? }` — paginated entity-string list
- `rope_getString { kind?, string_id }` — single descriptor
- `rope_globalStats` — `{ total_strings, total_knots, by_kind, invariant_holds }`

Every node enforces the invariant `count(strings) ≤ count(knots)`.

The **wallet** kind is fully active in production today. The **contract**, **asset**, **did**, and **cord** kinds are *protocol ready but unused* until each ecosystem agent starts emitting knots. This handover defines what each agent must emit, when, and with which payload shape.

---

## What is "a string"?

```
                ┌── kind = wallet  → 0xABC's history of personal-ledger entries
                ├── kind = contract → 0xDEF's history of structural changes (deploy, upgrade, fee policy…)
   string  ─────┤
                ├── kind = asset    → DCNFT://0xC/42's history of mints, transfers, valuations
                ├── kind = did      → ONCHAINID 0xID's history of identity claims
                └── kind = cord     → THE federation cord (one global string, anchor knots every ~3s)

   knot     ─── one signed, individually addressable, individually erasable
                event entry inside ONE string
```

Invariant: `total_knots ≥ total_strings` because every string starts with a genesis knot. The `rope_globalStats` response includes `invariant_holds: bool` — your test suite should assert it.

---

## Project-by-project responsibilities

### 1. Tanastok (`tanastok-app/`)

Tanastok issues two contracts per real-world asset (DCNFT title-deed + ERC-3643 share token). v1.2 asks Tanastok to mirror these on the string registry.

**Strings to create**

| Kind | id_bytes | Genesis trigger |
|---|---|---|
| `contract` | DCNFT contract address (20 bytes) | First call to `mint()` after deployment |
| `contract` | ERC-3643 contract address (20 bytes) | `init()` succeeds |
| `asset` | `keccak256("dcnft://<dcnft_addr>/<token_id>")` | Title-deed mint event |
| `asset` | `keccak256("erc3643://<erc3643_addr>")` | Token unpause |

**Knots to append after genesis**

| Event | Goes on which string | Suggested payload |
|---|---|---|
| Asset valuation update | `asset` (DCNFT) | `{ "kind":"valuation_update_v1", "old_value":…, "new_value":…, "evidence_uri":"…" }` |
| Compliance / claim issuance for an investor | `contract` (ERC-3643) | `{ "kind":"claim_issued_v1", "investor":"0x…", "topic":1, "issuer":"0xTanastokIssuer" }` |
| Pause / unpause | `contract` (ERC-3643) | `{ "kind":"pause_state_v1", "paused":true, "reason":"…" }` |
| Transfer of fractional shares above threshold (e.g. 1 % of supply) | `asset` (ERC-3643) | `{ "kind":"large_transfer_v1", "from":"0x…", "to":"0x…", "amount":"…" }` |

Per-investor ONCHAINID claim history goes on a `did` string keyed by the investor's ONCHAINID address.

### 2. DCSwap (`dcswap/`)

DCSwap is high-volume; we do **not** want a knot per swap. Knots represent *structural* events.

**Strings to create**

| Kind | id_bytes | Genesis trigger |
|---|---|---|
| `contract` | DCSwapRouter address | Router deployment |
| `contract` | DCSwapFactory address | Factory deployment |
| `asset` | Pair address (each LP pool) | `PairCreated` event |

**Knots to append**

| Event | Goes on | Cadence |
|---|---|---|
| Router or Factory upgrade (proxy `upgradeTo`) | the `contract` string | per upgrade |
| Fee-policy change | the `contract` string | per change |
| Pool listed / delisted | the pool's `asset` string | per listing |
| Daily volume aggregate (per pool) | the pool's `asset` string | once / 24h |
| Oracle TWAP checkpoint | the pool's `asset` string | hourly |

DCSwap should NOT mirror individual swaps. The Reth chain already has them; the registry is for entity-scoped digestible history.

### 3. NaturaProof

**Strings to create**

| Kind | id_bytes | Genesis trigger |
|---|---|---|
| `asset` | `keccak256("naturaproof://<plot_id>")` | First field measurement uploaded |
| `did` | Verifier's ONCHAINID address | First verifier accreditation |

**Knots to append**

| Event | Goes on | Notes |
|---|---|---|
| Field measurement | `asset` | Include hash of evidence pinned on IPFS |
| Third-party verification | `asset` | Signed by verifier's `did` string head knot |
| Certificate issued | `asset` | Mint of any associated DCR-20 carbon-credit token references this knot |
| Certificate revoked | `asset` | Use `rope_untieKnot` v1.1 for genuinely erasable evidence |
| Verifier accreditation update | `did` | |

### 4. Careaway

**Strings to create**

| Kind | id_bytes | Genesis trigger |
|---|---|---|
| `asset` | `keccak256("careaway://plan/<plan_id>")` | Plan creation |
| `did` | Beneficiary ONCHAINID address | Beneficiary enrolment (with explicit consent) |
| `did` | Caregiver ONCHAINID address | Caregiver onboarding |

**Knots to append**

Plan lifecycle (enrolment, care delivery, payout, dispute, closure) all land on the plan's `asset` string. Beneficiary-private events go on the beneficiary's `did` string and SHOULD use OES generation rotation so the beneficiary can later trigger granular erasure.

---

## How to emit a knot today

Until the v1.2.1 RPC extension `rope_appendToString { kind, id_bytes, … }` ships, agents have two options:

### Option A — wallet-keyed today, migrate later

Use the existing `rope_appendToLedger { wallet, record }` and pass the asset/contract address bytes in the `wallet` field. The registry will store these as `kind=wallet` for now. When v1.2.1 ships, the canonical agent will replay these into the correct kind via a one-time migration tool (we will publish it).

This is fine for *new* projects. NOT recommended for production-critical streams that you want filtered cleanly today.

### Option B — wait for v1.2.1 (recommended for production)

The `rope_appendToString` RPC will land in **late May 2026**. It mirrors `rope_appendToLedger` but takes an explicit `kind` argument. Migration from option A → option B is one-line in the calling code.

We'll send a follow-up handover when v1.2.1 ships.

---

## Reading the registry from your agent

Use the HTTP surface — same JSON, no auth required for reads:

```bash
# All strings of a given kind, paginated
curl 'https://erpc.datachain.network/api/v1/registry/strings?kind=asset&limit=50&offset=0'

# Global stats — assert the invariant in your CI
curl 'https://erpc.datachain.network/api/v1/registry/stats'
```

Or via JSON-RPC:

```bash
curl -X POST https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"rope_globalStats","params":[]}'

curl -X POST https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"rope_listStrings","params":[{"kind":"asset","offset":0,"limit":100}]}'
```

---

## Naming policy you must respect

Use **canonical v1.2 names** (`string_id`, `genesis_knot_id`, `head_knot_id`, `knot_count`, `kind`) in your own response shapes and frontends. The deprecated aliases (`wallet_address`, `genesis_string_id`, `head_string_id`, `entry_count`, `attestation_count`) ship in v1.2 responses but will be **removed in v1.3** (~2 release cycles, ~Q3 2026).

Do not introduce code that reads only the deprecated alias; review will reject it.

---

## Quick checklist for each agent

- [ ] Read `.cursor/rules/quipu-canon-v1.2-string-registry.mdc`
- [ ] Pick the strings your project owns (table in §1 above)
- [ ] Decide cadence for each event class (per event vs daily aggregate)
- [ ] If using Option A today, document the wallet → asset/contract mapping so the v1.2.1 migration tool can replay
- [ ] Add a CI assertion that `rope_globalStats.invariant_holds == true`
- [ ] Update your project's frontend to consume `/api/v1/registry/strings?kind=…` with your project's kind

---

*This handover defines a contract, not a deadline. The Datachain Rope chain works without ecosystem-side knots; v1.2 only ensures that when you do start emitting, the surface is canonical and stable.*
