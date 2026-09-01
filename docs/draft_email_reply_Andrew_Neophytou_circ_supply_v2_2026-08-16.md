# DRAFT v2 - Reply to Andrew Neophytou (Circ Supply) - 2026-08-16

**Status:** draft for operator send. Do not send language that contradicts the frozen fiat-revenue spec.

**Incoming:** Andrew, subject Circ Supply, 2026-08-16 ~14:56.

**Frozen comms constraints** (`handover-fiat-revenue-onchain-fat-purchase-spec-2026-08-13.mdc` + spec v1 §16):

- Do **not** tell Andrew that fiat-to-FAT buyback is live.
- Do **not** use **80%** as the ecosystem rate. Default is **50%** of fiat **net** revenue. Tanastok Private Pool USDC share stays **10%**. Combined 60% matches tokenomics eta. 80% is a per-product override that is not configured.
- The conversion ledger `0x000000000000000000000000000000000000d004` is the **watchable** string. It is labelled on dcscan. It becomes a live conversion log only after Phase 1 activation produces a real DCSwap `swap_tx`.

## Watch links to include (honest)

| Surface | URL | What it shows today |
|---|---|---|
| Circ / total supply | `https://dcscan.io/supply` | Scenario A circulating + demand-side **pending** card |
| Conversion API | `https://dcscan.io/api/v1/revenue-conversions` | `live: false`, `eta_fiat_to_fat: 0.50`, `conversions: []` |
| Conversion ledger | `https://dcscan.io/address/0x000000000000000000000000000000000000d004` | Label: Revenue FAT Conversion Ledger. Establishment knot after deploy. No buy knots yet. |
| Canonical FAT price | `https://dcswap.net/v1/prices` | `data.FAT.usd` |

## Demand-side wording (copy-safe)

SaaS fiat-to-FAT conversion is **designed and specified**, not running. When Phase 1 is approved, a share of settled fiat net revenue will buy DC FAT on the DCSwap FAT/USDC pair (no mint). Default share is 50%. Conversions will appear as knots on `0x…d004` and at `/api/v1/revenue-conversions`. Until then the ledger is the reserved watch address, not a live buyback contract.

Databox / node-operator rewards and Tanastok Private Pool lock-ups are separate, already-live demand sinks. Do not conflate them with the pending buyback.

## Do not say

- "We already convert subscription fees into DC."
- "80% of fees buy DC."
- "The d004 ledger is live converting."
- Any implication that Andrew's $10 WFAT purchase was a revenue buyback (that was the DCSwap on-ramp).
