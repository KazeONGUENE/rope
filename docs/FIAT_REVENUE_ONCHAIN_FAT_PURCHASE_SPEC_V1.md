# Fiat revenue to on-chain DC FAT purchase - Technical + Functional Specification v1

**Author:** Datachain Rope agent
**Workspace:** `/Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/`
**Date:** 2026-08-13
**Status:** SPEC FROZEN for a future implementation pass. **No code in this document. Do not implement from this file until an operator go-ahead names a phase.**
**Triggered by:** operator product decision (2026-08-13) that a share of fiat platform revenue should become a **real on-chain purchase of DC FAT**, recorded on Datachain Rope, across every ecosystem project that collects fiat. Adjacent context: Andrew Neophytou's 2026-08-12 question about an 80% subscription-fee buy-and-hold of DC (that flow is **not live today**).
**Canonical handle:** `FIAT_REVENUE_ONCHAIN_FAT_PURCHASE_SPEC_V1`
**Sister-agent pointer:** `.cursor/rules/handover-fiat-revenue-onchain-fat-purchase-spec-2026-08-13.mdc`

---

## 0. TL;DR

Today, fiat that lands in Tanastok / CareAway / similar products is booked in Postgres (and, for Tanastok Private Pool, paid out as **DCR-20 USDC**). Tanastok's `POST /api/payments/convert-to-dc` writes a `FIAT_TO_DC_CONVERSION` row using a USD price. **It never buys FAT on DCSwap and never anchors a knot.** The only production path that turns fiat into on-chain FAT is DCSwap's Revolut on-ramp: mint DCR-20 USDC to the minter, then `swapExactTokensForTokens` USDC -> WFAT with `to` = the buyer.

This spec makes the **revenue** analogue of that on-ramp:

1. Each participating project attests "this fiat net revenue settled" (signed, idempotent).
2. A shared **RevenueFatConverter** (no public mint key) batches those attestations.
3. It spends **already-reserved DCR-20 USDC on Rope** (preferred) through the FAT/USDC AMM and optionally unwraps WFAT to native FAT.
4. FAT lands in a labelled per-project **Revenue Conversion Treasury**.
5. A Quipu Canon v1.2 per-entity knot `FiatRevenueConvertedToFat` is anchored on a dedicated conversion ledger (`0x...d004`), with the AMM `Swap` tx hash in the metadata.

**It does not mint native FAT.** Minting FAT from fiat would be inflationary and is forbidden (same rationale as `fiat-mint-processor.ts`).

**It does not silently adopt Andrew's 80%.** Tokenomics eta is 60% of platform revenue to Private Pools. Tanastok live pool share is 10%, paid in USDC. This spec splits those into two independent levers so staker USDC yield is not stolen to fund the AMM buy.

| Lever | Default (this spec) | Live today | Tokenomics paper | Andrew 2026-08-12 |
|---|---|---|---|---|
| `eta_fiat_to_fat` (AMM buy of FAT) | **0.50** of fiat **net** revenue | 0 | not named as a buyback | 0.80 of **subscription** fee |
| `eta_pool_usdc` (USDC to stakers) | keep project default | Tanastok **0.10** | eta **0.60** to pools | not specified |
| Combined (buy + pool) | **0.60** | 0.10 | 0.60 | 0.80 if applied to all fiat |

Operator may override per project / per product class. Overrides are config, not code forks.

---

## 1. Why this exists

### 1.1 Product intent (operator, 2026-08-13)

> Converting the share of the revenue paid in fiat into DC FAT would make sense. The purchase of DC being recorded as an on-chain purchase of DC would make sense. This is something we should implement across the ecosystem of projects.

That is the requirement. Three properties are load-bearing:

| Property | Meaning | Failure mode if skipped |
|---|---|---|
| **Real purchase** | FAT leaves the AMM (or is unwrapped from WFAT that left the AMM). Supply of FAT is not increased. | Repeating `convert-to-dc`: a database number that looks like DC and is not |
| **On-chain record** | Anyone can verify amount, source project, period, and tx hashes on dcscan.io without trusting our DB | "We bought DC" becomes a slide |
| **Ecosystem-wide** | One converter + one knot schema. Tanastok, CareAway, later NaturaProof / Mapstore / etc. emit the same attestation | N copies of slightly different buyback bots |

### 1.2 What this is not

- Not a user on-ramp (DCSwap Revolut FAT buy already exists; leave it).
- Not the Arbitrum USDC bridge (still `phase: wiring`, BridgeMinter paused). Do not wait on it and do not unpause it for this feature.
- Not a change to DC FAT emission, genesis 10B, or asymptotic ~18B.
- Not a replacement of Private Pool USDC payouts unless the operator later sets `eta_pool_usdc = 0`.
- Not confirmation to Andrew that 80% is live. After Phase 1 ships, the honest line is: "fiat net revenue now buys FAT on DCSwap; the share is configured (`eta_fiat_to_fat`); every conversion is a knot on Rope."

### 1.3 Vocabulary (Quipu Canon)

| Term in this spec | Canon layer | Do not call it |
|---|---|---|
| AMM `Swap` tx | EVM-shaped **transaction** inside a cord anchor | "the knot" |
| `FiatRevenueConvertedToFat` | **Per-entity knot** on the conversion ledger string | "a block" / "a transaction" |
| Conversion treasury | `kind=wallet` string | "the pool contract" unless it actually is one |
| WFAT | DCR-20 wrapped native FAT (`0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4`) | "DCR-20 DC at `0x644...1441`" (that address is the FAT/USDT **LP**) |

Native staking-eligible DC-like value on chain 271828 is **native FAT** and **WFAT**. There is no separate DCR-20 DC token.

---

## 2. Current-state audit (evidence, 2026-08-13)

### 2.1 What already buys FAT on-chain from fiat

**DCSwap Revolut on-ramp** (`dcswap/bot/src/fiat-mint-processor.ts` + `dcswap/indexer/src/revolut.ts`):

- Stables (USDC / USDT / EUROD): `gatedMint` of BridgedToken to the **user**.
- FAT: mint the intermediate stable **to the minter wallet**, then `router.swapExactTokensForTokens(amountIn, minOut, [USDC, WFAT], recipient, deadline)`.
- Comment in source: minting FAT for fiat would be inflationary. `TARGET_TOKEN_ADDRESS.FAT = TOKENS.WFAT`.
- Slippage default 2%. Per-call cap ~ checkout max. Daily cap 5k (direct) / 25k (swap-minter aggregate).
- Indexer has **no** mint key. Bot polls `fiat_purchases.status = 'paid'`. CERBER halt file stops **mint**, not in-flight swaps (so as not to strand paid users).
- Compromised deployer `0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195` is refused by mint-gate.

**This is the only production-proven FAT buy path. The converter reuses it. It does not reimplement AMM math.**

### 2.2 Tanastok fiat revenue (the first emitter)

| Item | Live fact |
|---|---|
| Stakeholder subscriptions | Revolut checkout + scheduler + webhooks. Counted **100%** as platform revenue |
| Listing / share sale / pre-order / fiat-to-DC take | `DEFAULT_PLATFORM_TAKE_PCT = 5` |
| Private Pool budget | `DEFAULT_POOL_SHARE_PCT = 10` of **net** (`tanastok-app/src/lib/private-pool/platform-revenue.ts`) |
| Pool settlement | DCR-20 USDC from `0x63423bbc1275F973Eb00D6198B757797A8Db320B`, per-tx cap $5,000 |
| `POST /api/payments/convert-to-dc` | Verifies Stripe PI, `dcTokens = amount / dcPriceUsd`, Prisma `FIAT_TO_DC_CONVERSION`. **No swap, no knot** |
| Code "80%" | Returning-staker **month-remaining gate**, not a buy-DC rate |

### 2.3 Tokenomics vs live vs Andrew

From `docs/DC_FAT_Technical_Tokenomics_Specification_v1.1.md`:

```
P_RBF = (R_annual * eta) / (M_staked * APY_target)
eta = 0.60 (revenue allocation to Private Pools)
APY_target = 15-25%
```

P_RBF is a **pricing pillar**, not an instruction to buy FAT on the AMM. Paying stakers in USDC implements "eta of revenue to the pool" without creating AMM demand. Buying FAT with eta of revenue creates AMM demand but **cannot** also pay the same dollar out as USDC. Hence the split in section 4.

### 2.4 CareAway

`Careways_health_Connect/src/api/routes/dc-pool.ts` records `subscription_revenue_fiat` vs `subscription_revenue_usdc` for analytics. Ethereum / XDC legacy DC addresses appear. **No Rope AMM buy.** CareAway is emitter #2 after Tanastok.

### 2.5 Other fiat (or future fiat) projects

| Project | Fiat today | Converter role |
|---|---|---|
| DCSwap | User on-ramp (already buys FAT for the **user**) | Optional: convert **platform fee** (150 bps) the same way. Do not double-buy the user's FAT leg |
| NaturaProof, Mapstore, EDC, Syndicated | Little or no fiat rail yet | Same attestation schema when a rail exists |
| Datachain ID / dcscan / agents | No product fiat | Out of scope |
| Governance / VoteEscrow | FAT, not fiat | Out of scope |

### 2.6 On-chain USDC already usable (no bridge wait)

| Token | Address (271828) | Role |
|---|---|---|
| DCR-20 USDC | `0xb93bd8db94f1baff474aa9cba0739daaad01641f` | Preferred spend asset for the swap |
| DCR-20 USDT | `0x79a26132f48394421382c13b54ae77fa3af73289` | Allowed intermediate if USDC depth is thin |
| DCR-20 EUROD | `0x24d6137807fa8a592888726d87ac748d018c6d4a` | Allowed intermediate |
| WFAT | `0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4` | AMM output |
| FAT/USDC pair | `0xd9ebc3da001618a3ae90481d33ae7ef85e130317` | Canonical buy pool (zero-fee) |
| DCSwapRouter | `0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4` | `swapExactTokensForTokens` |
| Tanastok payout treasury | `0x63423bbc1275F973Eb00D6198B757797A8Db320B` | Has DCR-20 USDC today; **must not** be the conversion spender (payouts would race the buyback) |

Arbitrum `OriginBridgeVault` / Rope `BridgeMinter` stay paused. This feature does not depend on them.

### 2.7 Knot / auth rails to reuse

- Governance ledger `0x...d002`, node-requests `0x...d001`, databox `0x...d003` - pattern: JSONL or DB + `rope_appendToLedger` + rebuild-from-chain.
- Phase-2 signed destructive RPC is **live on BLUE**. Converter knots from a remote host MUST use EIP-191 envelopes. Loopback-without-XFF is only for a co-located worker on rope-vps.
- Address labels live in `crates/rope-explorer/src/main.rs::address_registry()` / `entity_labels`.

---

## 3. Design decisions (frozen vs operator-gated)

### 3.1 Frozen (implementers must not reopen without a spec revision)

| ID | Decision | Rationale |
|---|---|---|
| D1 | Never mint native FAT or WFAT to satisfy this flow | Inflation. Same as on-ramp |
| D2 | Buy path is DCSwap AMM: USDC (or USDT/EUROD) -> WFAT | Only live FAT market on Rope |
| D3 | Default `destination_form = native_fat` after `WFAT.withdraw` | Matches MetaMask "DC FAT" and `eth_getBalance` |
| D4 | FAT recipient is a **per-project labelled treasury**, not staker pro-rata, not burn | "Purchase recorded" != "yield token switched to FAT" |
| D5 | `eta_fiat_to_fat` and `eta_pool_usdc` are **independent** | One dollar cannot both buy FAT and pay USDC |
| D6 | Shared converter; projects only emit attestations | One minter/swapper blast radius, one halt flag |
| D7 | Indexer/API process holds **no** swap or mint key | Copy on-ramp split |
| D8 | Batch, do not swap per $50 webhook | AMM depth, gas, MEV, audit clarity |
| D9 | Idempotency key required; replay is a no-op success | Webhook retries |
| D10 | Compromised deployer `0x60FB...4195` refused as signer, minter, and treasury | Standing security rule |
| D11 | Knot type `FiatRevenueConvertedToFat` on ledger `0x...d004` | Searchable, distinct from governance / node-requests / databox |
| D12 | BridgedToken USDC mint for this flow is **opt-in and attested**, not default | Default spends USDC the project already holds on Rope |
| D13 | CERBER halt stops **new** conversion batches; in-flight swaps complete | Same philosophy as on-ramp swap phase |
| D14 | No stubs, no simulated fills, no "credit FAT in DB then settle later" | Workspace production-ready rule |

### 3.2 Operator-gated (spec recommends a default; config may override)

| ID | Question | Spec default | Allowed overrides |
|---|---|---|---|
| O1 | `eta_fiat_to_fat` | **0.50** of fiat **net** revenue | Per project, per `product_class` (e.g. Tanastok `subscription` = 0.80 if you want Andrew's number) |
| O2 | `eta_pool_usdc` | **unchanged** (Tanastok 0.10) | Raise toward 0.60 later; never auto-cut to fund O1 |
| O3 | Batch cadence | **Daily 00:00 UTC** close + run at 00:30 UTC | `hourly` / `weekly` |
| O4 | Min notional per batch | **100 USD** equivalent | Skip and accrue if below |
| O5 | Max notional per AMM tx | **5,000 USD** then TWAP-split | Align with Private Pool per-tx cap |
| O6 | Daily converter cap | **25,000 USD** notional (on-ramp swap-minter cap) | Raise via env after soak |
| O7 | Slippage | **2%** vs `getAmountsOut` | Halt batch if quote vs `dcswap.net/v1/prices` FAT USD diverges > 5% |
| O8 | `destination_form` | `native_fat` | `wfat` (skip unwrap) |
| O9 | Treasury key custody | Fresh EOA per project, key only on converter host, 0600 | Later: Timelock / Safe as recipient (`to` can be a contract that receives ERC-20; unwrap to a contract needs a payable receiver - if treasury is a contract, keep `wfat`) |
| O10 | USDC source | **Pre-funded project USDC on Rope** | `mint_reserved` only with written 1:1 fiat-reserve attestation |
| O11 | First emitter | **Tanastok** (subscriptions + listing fees) | CareAway phase 2 |

### 3.3 Recommended default vs Andrew 80%

Implement **O1 = 0.50** ecosystem-wide so that:

```
eta_fiat_to_fat (0.50) + eta_pool_usdc (0.10 live) = 0.60 tokenomics eta
```

If the operator wants Andrew's subscription story to become literally true:

```
Tanastok product_class=subscription  eta_fiat_to_fat = 0.80
Tanastok product_class=listing_fee   eta_fiat_to_fat = 0.50  (or 0.00 if listing already feeds the pool)
```

That 80% comes **out of Tanastok's fiat ops margin**, not out of the 10% USDC pool, unless the operator explicitly lowers `eta_pool_usdc`. The spec will not infer that cut.

---

## 4. Functional requirements

### 4.1 Actors

| Actor | Does |
|---|---|
| **Paying user** | Pays fiat (Revolut / Stripe / bank). Unchanged UX. They do **not** receive the FAT from this flow |
| **Project emitter** (Tanastok, CareAway, ...) | After fiat settlement is final, POST a signed `RevenueSettled` attestation. Never holds the converter swap key |
| **RevenueFatConverter** | Polls attestations, applies eta, checks caps/oracle/halt, swaps, optional unwrap, writes receipt, requests knot |
| **Knot worker** | `rope_createPersonalLedger` (idempotent) + Phase-2 `rope_appendToLedger` on `0x...d004` |
| **DCScan** | Labels treasuries; `/api/v1/revenue-conversions` reads knots + receipt store |
| **CERBER** | Halt flag + optional rule on converter miss / oracle divergence |
| **Operator** | Sets eta, funds USDC, labels, halt, cap raises |

### 4.2 User-visible behaviour (after Phase 1)

- Paying for a Tanastok subscription still charges fiat and still unlocks the product.
- Within one UTC day (plus converter runtime), `eta_fiat_to_fat` of that day's **net** subscription (+ in-scope fees) appears as:
  - an AMM swap on dcscan (`/tx/<swap_hash>`), USDC in / WFAT out, `to` = project conversion treasury (or WFAT then unwrap tx),
  - a knot on `https://dcscan.io/address/0x000000000000000000000000000000000000d004` (and the treasury's own string once labelled).
- Private Pool USDC payouts continue on their existing cadence unless O2 is changed.
- Public dashboard (Phase 3): per-project "fiat converted this month", FAT bought, USD notional, eta used.

### 4.3 Net revenue definition (must be identical to each project's books)

```
gross_fiat          = amount charged to the user in original currency
processor_fees      = Revolut/Stripe/FX fees (positive number)
refunds_chargebacks = settled reductions in the same period
net_fiat_usd        = fx(gross_fiat - processor_fees - refunds_chargebacks)
convertible_usd     = net_fiat_usd * eta_fiat_to_fat   # eta from config for (project, product_class)
```

Rules:

- Use the **same FX** the project already books (Tanastok `dc_token_prices` / existing revenue FX). Do not invent a second FX.
- `product_class` examples: `subscription`, `listing_fee`, `share_sale_take`, `preorder_take`, `careaway_subscription`. Eta is keyed `(project_id, product_class)`.
- **Do not** convert the user's own `convert-to-dc` principal. That fiat is customer funds, not platform take. Only the **platform take** (Tanastok 5% on that product) may enter this flow.
- **Do not** convert DCSwap on-ramp notional. Only DCSwap **fee** (if O11 includes it).

### 4.4 Destination policy

| `destination_policy` | Behaviour | Default |
|---|---|---|
| `treasury_hold` | FAT sits in the labelled conversion treasury | **yes** |
| `auto_stake` | Out of scope for v1 (would mix protocol-owned FAT into `M_staked` and distort P_RBF / APY) | no |
| `burn` | Out of scope for v1 (conflicts with "hold / purchase" language and emission narrative) | no |

v1 is **buy and hold**, publicly labelled, so Andrew's "is it just held in a wallet?" becomes **yes, on this wallet, here is the dcscan link**, with every refill a knot.

### 4.5 Failure behaviour (honest, never fake-complete)

| Failure | User product | Converter | Knot |
|---|---|---|---|
| Fiat webhook delayed | Existing retry | No attestation yet | None |
| Attestation invalid / bad sig | Product already granted (fiat settled) | Reject 4xx, no swap | None |
| USDC balance too low | Unchanged | Accrue `blocked: insufficient_usdc`, page operator | None until funded |
| AMM slippage / oracle halt | Unchanged | Retry next cycle; do not widen slippage silently | None |
| Swap mined, unwrap fails | Unchanged | WFAT already in treasury; unwrap retries; receipt `status=swapped_wfat` | Knot **after** swap (form=`wfat`), follow-up knot on unwrap |
| Knot append fails, swap ok | Unchanged | Receipt `status=swapped_knot_pending`; retry knot only | Must land; swap is the economic event |

Never mark `status=converted` in the emitter DB until the converter returns `swap_tx_hash`. Emitter may show `conversion_pending`.

---

## 5. Technical architecture

### 5.1 Topology

```
Tanastok / CareAway / ...                dcswap-prod                         rope-vps
-------------------------                -----------                         --------
fiat webhook (existing)                  RevenueFatConverter                 knot worker
        |                                (poll, no public HTTP               (loopback
        v                                 mint/swap key on                    Phase-2 or
signed RevenueSettled ----------------->  the internet-facing                 X-Rope-Internal)
POST /internal/revenue-settled            indexer)                                  |
(indexer, no keys)                              |                                   |
        |                                       v                                   v
   202 + attestation_id              USDC.approve(Router)              rope_appendToLedger
                                     swapExactTokensForTokens          FiatRevenueConvertedToFat
                                     optional WFAT.withdraw            ledger 0x...d004
                                     write receipt JSONL/PG
```

Split of privilege (copy the on-ramp):

| Process | Host | Secrets | Listens publicly |
|---|---|---|---|
| Attestation ingest | project's API **or** dcswap indexer | **none** for swap; verifies project Ed25519/EIP-191 | Yes, rate-limited |
| Converter | dcswap-prod (recommended: next to `fiat-mint-processor`) | USDC/WFAT spender key(s); **not** BridgedToken minter unless O10=`mint_reserved` | No (poll + localhost metrics) |
| Knot worker | rope-vps | Phase-2 wallet key for `0x...d004` **or** loopback | No |

Do **not** put the conversion spender key on tanastok-vps next to the public Next.js app.

### 5.2 Why dcswap-prod for the swap

- Router, pairs, `gatedMint`, retry, halt-flag, and CERBER are already there.
- FAT/USDC depth lives on Rope; dcswap-prod already talks to `erpc.datachain.network` with write-pin awareness.
- Tanastok remains an **emitter**, not a second AMM bot.

If the operator later wants the converter on rope-vps, the same binary/module moves; the attestation schema does not change.

### 5.3 USDC funding (two modes)

**Mode A - `prefunded` (default, v1 launch)**

Each project keeps a **Conversion USDC float** on Rope (separate wallet from Private Pool payout treasury). Ops tops it up from:

- existing DCR-20 USDC the project already holds, or
- DCSwap on-ramp / OTC, or
- (later) the stablecoin bridge **after** it is actually live.

Converter only `transferFrom` or spends from **its** hot wallet that projects have funded (project -> converter USDC transfer, attested).

Simplest v1: **one converter hot wallet** funded by Foundation/ops; internal accounting attributes FAT bought to `project_id`. Still one on-chain `to` per project treasury for the FAT output (`swap` `to` = project treasury). USDC in can be converter-owned.

**Mode B - `mint_reserved` (not default)**

`gatedMint` USDC to the converter against attested fiat in Revolut/bank, 1:1, then swap. Economically: USDC supply up, USDC goes into the AMM, FAT out to treasury. Allowed **only if**:

- BridgedToken minter is Timelock-controlled,
- `0x60FB...` is not the minter,
- a signed reserve attestation (`fiat_reserve_usd >= minted_usdc`) is stored with the receipt,
- daily mint cap equals O6.

v1 ships Mode A so the feature cannot become a silent USDC printer.

### 5.4 Swap execution (normative)

Reuse `fiat-mint-processor.ts` Phase 2, with these differences:

| On-ramp | Converter |
|---|---|
| One order <-> one swap | Many attestations -> one or more TWAP swaps |
| `to` = paying user | `to` = project conversion treasury |
| Intermediate minted that cycle | Intermediate already held (Mode A) |
| Halt does not stop swaps | Halt **does** stop **new** converter swaps (revenue can wait; users cannot) |

Algorithm per batch `(project_id, utc_date)`:

1. Sum `convertible_usd` for `status=attested` rows not yet in a batch.
2. If sum < O4, exit (accrue).
3. Convert USD -> USDC 6-decimal using oracle (`dcswap.net/v1/prices` USDC ~1). `usdc_in = round_down(convertible_usd * 1e6)`.
4. Cap `usdc_in` by O6 remaining and by wallet `balanceOf`.
5. Split into chunks <= O5.
6. For each chunk: `getAmountsOut`; require quote FAT USD within 5% of canonical `data.FAT.usd`; `minOut = quoted * (100 - slippage) / 100`; `swapExactTokensForTokens`.
7. Parse `Swap` amountOut (same `extractSwapOutAmount` helper).
8. If `destination_form=native_fat` and treasury is an EOA: `WFAT.withdraw(amount)` **from the treasury** (treasury must hold the WFAT). That implies either:
   - **Preferred:** `swap` `to` = converter, then `WFAT.withdraw`, then native FAT `transfer` to treasury (two extra txs; converter briefly holds FAT - keep window < 1 block by sending in the same script loop), **or**
   - `swap` `to` = treasury, then a **treasury-keyed** `withdraw` (second key on converter host, per project).

   Spec freeze for v1: **swap `to` = conversion treasury (WFAT)**, then converter uses the **per-project treasury key** to `withdraw` to native FAT in the same wallet (unwrap in place). No FAT hops across addresses. Keys live only on the converter host.

9. Write receipt. Enqueue knot.

Gas: conversion treasuries need a native FAT dust float for `withdraw` (Tanastok payout treasury lesson: USDC without FAT cannot send). Ops pre-funds **0.05 FAT** gas on each conversion treasury.

### 5.5 Oracle and circuit breakers

| Check | Source | Action on fail |
|---|---|---|
| FAT USD | `https://dcswap.net/v1/prices` -> `data.FAT.usd` (canonical v2.1, no floor) | Skip batch |
| Spot vs pool | `getAmountsOut` implied price vs canonical, max 5% | Skip chunk, page |
| Pool reserves | `getReserves` on FAT/USDC; reject if USDC reserve < 2x chunk | Split smaller or skip |
| RPC writer | `fleet-status.writer.status=healthy` AND `edge.status!=down` | Skip (do not swap onto a failover attester) |
| Halt file | same path as `fiat-mint-processor` CERBER halt | Skip new batches |

Writes (`eth_sendRawTransaction`) must hit BLUE. Converter uses `https://erpc.datachain.network` only (published RPC). If ghost-tx reclaim is needed, existing HA handles it; converter treats "hash then dropped" as retry (status stays `swap_broadcast`).

### 5.6 Persistence

**Attestation store** (emitter or shared ingest DB):

```
revenue_settled (
  attestation_id uuid pk,
  project_id text,
  product_class text,
  idempotency_key text unique,  -- e.g. tanastok:revolut_order:ord_...
  net_fiat_usd numeric,
  eta_bps int,
  convertible_usd numeric,
  settled_at timestamptz,
  payload jsonb,                -- invoice ids, currency, fx
  status text,                  -- attested | batched | converted | blocked | rejected
  batch_id uuid null,
  created_at timestamptz
)
```

**Receipt store** (converter, source of truth for amounts):

```
revenue_fat_receipts (
  receipt_id uuid pk,
  batch_id uuid,
  project_id text,
  utc_date date,
  usdc_in_raw numeric,
  wfat_out_raw numeric,
  native_fat_out_wei numeric,
  swap_tx_hash text,
  unwrap_tx_hash text null,
  knot_id text null,
  status text,  -- swapped_wfat | unwrapped | knot_pending | converted | failed
  created_at timestamptz
)
```

Chain is canonical for the purchase; DB is an index. If DB is wiped, rebuild receipts from FAT/USDC `Swap` where `to` in the labelled treasury set, plus knots on `0x...d004`.

---

## 6. Wire format

### 6.1 `RevenueSettled` attestation (project -> ingest)

HTTP `POST /v1/revenue/settled` on the ingest host (dcswap indexer or a Rope route). CORS not required (server-to-server).

```json
{
  "body": {
    "kind": "revenue_settled",
    "schema": "datachain.revenue-settled/v1",
    "project_id": "tanastok",
    "product_class": "subscription",
    "idempotency_key": "tanastok:revolut:ord_01J...",
    "settled_at": 1786636800,
    "gross_minor": 9900,
    "gross_currency": "EUR",
    "fx_usd": "1.084000",
    "processor_fee_usd": "0.32",
    "refund_usd": "0",
    "net_fiat_usd": "10.41",
    "eta_bps_hint": 5000,
    "source_refs": {
      "revolut_order_id": "ord_01J...",
      "tanastok_invoice_id": "inv_..."
    }
  },
  "envelope": {
    "scheme": "ed25519-cerber-mesh-v1",
    "peer_id": "tanastok-revenue",
    "kid": "<16-hex>",
    "public_key": "<64-hex>",
    "kind": "revenue_settled",
    "signed_at": 1786636810,
    "nonce": "0x<32-hex>",
    "signature": "0x<128-hex-ed25519>",
    "body_sha256": "<64-hex canonical body>"
  }
}
```

Canonicalisation: same deterministic JSON + domain tag as CERBER mesh (`DCROPE/revenue-settled/v1\0`), so `cerber-sign.mjs` can be reused. Alternative for EVM-key projects: `scheme: secp256k1-eip191` recovering to a pinned project ops address.

Ingest rules:

- Unknown `project_id` / unpinned key -> 401.
- `eta_bps_hint` is **advisory**. Converter applies **server-side config** eta. Mismatch -> warn in receipt, do not trust the hint.
- Duplicate `idempotency_key` + identical body -> 200 with existing `attestation_id`.
- Duplicate key + different body -> 409.
- `net_fiat_usd` must equal `gross*fx - fees - refunds` within 1 cent or 409.
- Max body 16 KB. Rate limit 2 r/s per project.

Response `202`:

```json
{
  "ok": true,
  "attestation_id": "uuid",
  "status": "attested",
  "eta_bps_applied": 5000,
  "convertible_usd": "5.21"
}
```

### 6.2 Converter receipt (internal, also published)

After a successful batch, ingest `GET /v1/revenue/receipts/:batch_id` (internal ACL) returns the receipt row plus tx hashes. Projects may poll to flip `conversion_pending` -> `converted`.

### 6.3 Knot payload

`rope_appendToLedger(0x...d004, interaction)`:

```json
{
  "interaction_type": "FiatRevenueConvertedToFat",
  "description": "Tanastok subscription fiat net converted to DC FAT via DCSwap FAT/USDC.",
  "metadata": {
    "schema": "datachain.fiat-revenue-fat/v1",
    "project_id": "tanastok",
    "product_classes": ["subscription"],
    "utc_date": "2026-08-13",
    "batch_id": "uuid",
    "eta_bps": 5000,
    "net_fiat_usd": "1041.00",
    "convertible_usd": "520.50",
    "usdc_in": "520.500000",
    "usdc_contract": "0xb93bd8db94f1baff474aa9cba0739daaad01641f",
    "wfat_out": "12345.678901234567890123",
    "native_fat_out": "12345.678901234567890123",
    "destination_form": "native_fat",
    "treasury": "0x...",
    "pair": "0xd9ebc3da001618a3ae90481d33ae7ef85e130317",
    "router": "0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4",
    "swap_tx": "0x...",
    "unwrap_tx": "0x...",
    "price_usd_canonical": "0.0323",
    "price_source": "dcswap-reserves(outlier-rejected-gecko)",
    "attestation_ids": ["uuid", "..."]
  }
}
```

Description prefix is stable so `semantic-agent` `/v1/search?q=FiatRevenueConvertedToFat` ranks these knots.

Phase-2: last `params` element is the EIP-191 auth envelope; signer must be the `0x...d004` key.

Monthly rollup (optional Phase 3): one knot `FiatRevenueConvertedToFatRollup` with month totals. Daily knots remain.

---

## 7. Identities, labels, ledgers

| Role | Address | Label on dcscan |
|---|---|---|
| Conversion ledger | `0x000000000000000000000000000000000000d004` | `Revenue FAT Conversion Ledger` |
| Tanastok conversion treasury | **new EOA** (not `0x63423bbc...320B`) | `Tanastok Fiat Revenue FAT Treasury` |
| CareAway conversion treasury | new EOA | `CareAway Fiat Revenue FAT Treasury` |
| Converter USDC hot wallet | new EOA on dcswap-prod | `Ecosystem Revenue Converter (USDC)` |

`0x...d004` is unused as of 2026-08-13 (`d001` node-requests, `d002` governance, `d003` databox). Confirm with a grep before deploy.

Private Pool payout treasury **stays** USDC-only. Mixing buyback and payouts on one wallet makes accounting and halt behaviour unsafe.

---

## 8. Security

### 8.1 Threat model (short)

| Threat | Mitigation |
|---|---|
| Attestation forgery -> drain USDC into AMM for attacker treasury | Pinned project keys; `treasury` comes from **server config**, not the body |
| Replay | `idempotency_key` + envelope nonce |
| Inflated `net_fiat_usd` | Arithmetic check; optional later: ingest verifies Revolut/Stripe server-side like on-ramp `verifyPaidOrder` |
| Converter key stolen | Caps O5/O6; halt file; USDC float sized to a few days not months; no BridgedToken minter on this key in Mode A |
| Mint USDC without fiat (Mode B) | Not in v1; if enabled, reserve attestation + Timelock minter |
| Swap on attester, ghost tx | Writer-healthy gate; existing ghost reclaim |
| Compromised deployer | Refused everywhere |
| Insider eta=100% | Config in env on converter host, 0640, change is an ops event not an API field |
| Knot without swap / swap without knot | Receipt state machine; DCScan flags `swap_tx` missing on a knot |

### 8.2 Caps (v1 numbers)

Copied from on-ramp unless noted:

| Cap | Value |
|---|---|
| Per AMM tx notional | 5,000 USD |
| Per UTC day notional (all projects) | 25,000 USD |
| Per project per day | 10,000 USD |
| Slippage | 2% |
| Oracle divergence | 5% |
| Attestation body | 16 KB |
| Attestations per batch | 500 |

Exceeding a cap **accrues**, does not haircut silently. Operator is paged (`contact@onguene.com` / CERBER).

### 8.3 Halt

File path: reuse DCSwap `HALT_FLAG` **or** a dedicated `/var/lib/cerber/revenue-fat-halt`. Presence -> converter poll returns immediately. Ingest still accepts attestations (`status=attested`). Product UX unchanged.

### 8.4 Key ceremony

1. Generate conversion treasury EOAs on an air-gapped or converter-host-only path (`ethers.Wallet.createRandom()` on dcswap-prod, like Tanastok payout treasury).
2. Persist keys in converter `EnvironmentFile=` mode 0600. Never argv, never git, never `.cursor/*.json`.
3. Fund gas 0.05 FAT.
4. Label on dcscan before first swap.
5. `rope_createPersonalLedger` for treasury + `0x...d004`.
6. Anchor a `RevenueConversionTreasuryEstablished` knot on the treasury string (same pattern as `PrivatePoolTreasuryEstablished`).

---

## 9. Per-project integration (emitters)

### 9.1 Shared contract (every project)

1. Pin an Ed25519 or EIP-191 key with Rope/DCSwap ingest.
2. After **final** fiat settlement (not `pending`), POST `RevenueSettled`.
3. Store `attestation_id` on the invoice row.
4. Do not compute FAT amounts locally for user display of **this** flow (avoid a second price). Optional: poll receipt for an explorer link.
5. Never send customer principal, only platform net.

### 9.2 Tanastok (Phase 1)

Hook points (do not implement in this spec pass):

- Revolut subscription webhook after status is paid and invoice is immutable.
- Listing-fee success path in `platform-revenue.ts` (the **take**, not the seller's proceeds).
- **Exclude** `convert-to-dc` principal; include only `DEFAULT_PLATFORM_TAKE_PCT` of that product if the operator wants that take in the buy.

Keep `DEFAULT_POOL_SHARE_PCT = 10` USDC path untouched in Phase 1.

### 9.3 CareAway (Phase 2)

Emit from the code path that already increments `subscription_revenue_fiat`, once per settled invoice, USD net after processor fees.

### 9.4 DCSwap (optional Phase 2)

If converting the **150 bps on-ramp fee**: emit `product_class=onramp_fee`, `net_fiat_usd = fee only`. Never the user's FAT buy notional.

### 9.5 Future projects

Same schema. Adding a project is: pin key, allocate treasury, set eta row, label, fund USDC float + gas. No converter fork.

---

## 10. DCScan and observability

### 10.1 Explorer (Phase 1 minimum)

- Address pages for conversion treasuries: gold/black design-system reserve styling optional; label + "FAT bought from fiat revenue" note + link to ledger `0x...d004`.
- `GET /api/v1/revenue-conversions?project=tanastok&from=&to=` - rebuilt from knots (decrypt/repatriate if needed) + optional receipt mirror. Public, no key. Cache SWR 60s.

**Substrate shipped 2026-08-16 (observability only, converter still gated):** dcscan labels `0x...d004` as `Revenue FAT Conversion Ledger`; `GET /api/v1/revenue-conversions` is live and returns `live: false` / `phase: pending` / `eta_fiat_to_fat: 0.50` until a real `FiatRevenueConvertedToFat` knot with `swap_tx` exists; `/supply` shows the same pending card. This is **not** Phase 1 activation.

### 10.2 Metrics (converter)

Log JSON lines (no PII beyond project_id):

```
converted project=tanastok usd=520.50 usdc=520.5 fat=12345.67 swap=0x... knot=...
skipped reason=oracle_divergence|insufficient_usdc|halt|writer_unhealthy|below_min
```

CERBER (Phase 2): page if `skipped` for `insufficient_usdc` persists > 24h or if no `converted` in 3 days while attestations accrue.

---

## 11. Rollout phases (implementation later)

| Phase | What | Gate to start | Gate to call done |
|---|---|---|---|
| **0** | This spec + operator answers to section 15 | Written | Operator ack on O1-O11 |
| **1** | Converter + ingest + Tanastok subscriptions (+ listing take) + labels + knots + Mode A USDC | Phase 0 | 7 days live, >=1 real batch, dcscan shows knot + swap, pool USDC still paying |
| **2** | CareAway emitter; optional DCSwap fee; public `/api/v1/revenue-conversions`; CERBER rule | Phase 1 soak | Second project converting |
| **3** | Dashboard on dcscan `/ecosystem` or `/supply`; monthly rollup knots; TWAP tuning | Phase 2 | Operator |
| **4** | Mode B `mint_reserved` only if Mode A float becomes operationally painful | Written reserve policy | Audit of 1:1 fiat |

Do not combine Phase 1 with BridgeMinter unpause, P2B experiments, or Tanastok pool-share 10% -> 60% in the same deploy window.

---

## 12. Test plan (when code exists)

No mocks of AMM fills. Tests use a local Anvil/Reth fixture **or** fork-style Foundry only if the suite already does; prefer:

| Layer | Tests |
|---|---|
| Net/eta math | Table: refunds, FX, take-vs-principal, hint ignored |
| Idempotency | Duplicate POST; conflicting body |
| Caps | Chunk split at 5000; daily stop and accrue |
| Oracle | Divergence >5% skips |
| Swap | Integration against DCSwap pair on a test chain with real Router ABI; assert `Swap` logs and `to` |
| Unwrap | WFAT.withdraw increases `eth_getBalance(treasury)` by amount |
| Knot | Phase-2 signed append; unsigned denied `-32401` |
| Halt | File present -> no new swap |
| Rebuild | Wipe DB, reconstruct from logs + knots |
| Regression | `0x60FB` signer refused; payout treasury not used as `to` |

`cargo` / Foundry / Node tests must be real. Forbidden: `status=converted` in a unit test without asserting a tx hash format and a mocked-but-explicit RPC layer that still exercises the state machine. Prefer one integration test that broadcasts on a ephemeral local chain.

---

## 13. Non-goals

- Changing DC FAT emission or claiming this buy **burns** supply.
- Moving Private Pool yield from USDC to FAT in v1.
- Auto-staking bought FAT.
- Using `0x644da44bcd5f453c593781dbe22dfd733e8d1441` as a DC token (it is FAT/USDT LP).
- Unpausing Arbitrum vault / BridgeMinter.
- Telling Andrew 80% is live before Phase 1 **and** an O1 override of 8000 bps on `subscription`.
- Per-payment swaps.
- Letting the attestation choose the treasury.
- Minting WFAT/FAT.
- Putting swap keys on tanastok-vps.

---

## 14. Future implementation inventory (do not create until Phase 1)

| Likely path | Role |
|---|---|
| `dcswap/bot/src/revenue-fat-converter.ts` | Poll + swap + unwrap (sibling of `fiat-mint-processor.ts`) |
| `dcswap/indexer/src/revenue-settled.ts` | Ingest + verify + persist |
| `tanastok-app/src/lib/revenue/emit-settled.ts` | POST after Revolut paid |
| `datachain-rope/crates/rope-explorer/src/revenue_conversions.rs` | Public read API from knots. **Shipped 2026-08-16 as pending/empty substrate** (not converter). |
| `datachain-rope/crates/rope-explorer/src/main.rs` `address_registry` | Labels. **d004 labelled 2026-08-16.** |
| systemd `revenue-fat-converter.service` on dcswap-prod | Loop |
| `/etc/revenue-fat-converter.env` | Eta table, keys, caps (0600) |

Exact filenames may change; the **behaviour** in this spec must not.

---

## 15. Asks of the operator (block Phase 1 code)

Reply in a handover or in-session; defaults in section 3.2 apply if you say "defaults".

1. Confirm **O1 = 0.50** ecosystem default, and whether Tanastok **`subscription` is 0.80** (Andrew) or 0.50 like everything else.
2. Confirm **O2**: leave Tanastok pool at 10% USDC for Phase 1.
3. Confirm **Mode A** (pre-funded USDC) for launch.
4. Confirm **new EOAs** for conversion treasuries (not the Private Pool payout wallet).
5. Confirm converter host = **dcswap-prod** (recommended) vs rope-vps.
6. Name who funds the first USDC float (Foundation vs Tanastok vs DCSwap) and the starting size (recommendation: **$15,000** USDC, ~3 days at O6).
7. Whether CareAway is in Phase 1 or waits for Phase 2.

---

## 16. Comms constraint (Andrew and public)

Until Phase 1 is live-verified (real `swap_tx` + knot on `0x...d004`):

- Do **not** say we convert subscription fees into DC.
- Do **not** say 80%.

After Phase 1, allowed one-liner:

> A configured share of settled fiat net revenue is sold into DC FAT on DCSwap (USDC -> WFAT, then unwrap). The FAT is held in a labelled treasury. Each day's conversion is a knot on Datachain Rope. Private Pool USDC payouts are a separate share.

If O1 for subscriptions is 8000 bps, that sentence may say "80% of Tanastok subscription net revenue".

---

## 17. Cross-references

| Doc / code | Why |
|---|---|
| `dcswap/bot/src/fiat-mint-processor.ts` | Swap path, caps, halt philosophy |
| `tanastok-app/src/lib/private-pool/platform-revenue.ts` | Live 10% / 5% / 100% subscription |
| `docs/DC_FAT_Technical_Tokenomics_Specification_v1.1.md` | eta 60%, P_RBF |
| `.cursor/rules/dcr20-token-standard.mdc` | DCR-20 naming |
| `.cursor/rules/handover-from-tanastok-treasury-and-rope-token-reconciliation-2026-06-04.mdc` | `0x644...` is LP, not DC |
| `.cursor/rules/handover-security-audit-2026-06-11.mdc` | Phase-2 signed `rope_appendToLedger` |
| `.cursor/rules/quipu-canon-v1.2-knot-event-distinction.mdc` | Knot vs tx vs event |
| `docs/EDGE_EXTERNAL_PROBES_INGEST_SPEC_v1.md` | Signed envelope pattern to copy |
| `handover-governance-voting-platform-phase1-live-2026-07-22.mdc` | JSONL + ledger wallet pattern |

---

*Canonical handle: `FIAT_REVENUE_ONCHAIN_FAT_PURCHASE_SPEC_V1`. Implementation starts only after section 15 is answered and a Phase 1 go-ahead is explicit. This file is the source of truth for that future pass.*
