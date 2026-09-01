# Outreach drafts - MintMe / XSwap / CoinGecko (2026-08-14)

**Prerequisite:** the guide is live at <https://dcscan.io/apis/exchange-integration> (deploy notes in `EXCHANGE_INTEGRATION_PUBLISH_2026-08-14.md`).

All three messages assume the operator sends them from a `datachain.one` / `datachain.network` address so replies land on `contact@datachain.one`. Nothing below is auto-sent - drop into the mail client / Telegram of your choice and hit send.

---

## 1. MintMe - `contact@mintme.com`

**Subject:** Datachain (FAT) listing on MintMe - integration guide + technical package

Hi MintMe listing team,

I am reaching out on behalf of Datachain Foundation SAS about listing Datachain (ticker `FAT`) on MintMe. FAT is the native coin of Datachain Rope, a fully EVM-compatible L1 (chainId `271828`) that has been in production since 2026-03-25 and is currently tracked on CoinMarketCap as id `10357`.

Because Datachain Rope is EVM-compatible in the same way BASE, Arbitrum, Polygon and Avalanche (already on MintMe) are, adding FAT is a one-row chain config change on your side. No bespoke SDK, no proprietary API, no custom cryptography for the wallet layer.

We prepared a production integration guide sized for exchange engineering:

**<https://dcscan.io/apis/exchange-integration>**

It contains everything your team needs:
- Section 2: paste-safe network parameters (chain name, chain id, RPC + WebSocket URLs, block time, finality depth, HD derivation path).
- Section 3: the standard `eth_*` JSON-RPC surface. Every method your existing EVM integrations use is available. Two Rope-specific hardening notes for exchange withdrawal broadcast (`self_heal.recommended_deadline_padding_secs` and the ghost-tx reclaim behaviour).
- Section 4: the block explorer JSON API on `dcscan.io/api/v1/*`, including the two bare-text `text/plain` endpoints CMC polls for circulating and total supply.
- Section 5: the full asset registry - native FAT (what you list), wrapped WFAT (DCR-20, ERC-20 wire-compatible), legacy DC tokens on Ethereum and XDC (the 1:1 audited burn-and-mint migration source), and bridged stablecoins.
- Section 7: a MintMe-specific playbook with every field your listing form asks for, pre-filled.

Two operational commitments from us:

1. **Market-maker.** Datachain Foundation will operate a two-sided MM on the MintMe FAT/USDT pair (and FAT/BTC when live), redirecting the existing 62-wallet / 9-strategy DCSwap bot at your order book. Depth commitment: at least the same book depth we run on our own DCSwap DEX (see <https://dcswap.net> - real-time reserves for reference).
2. **Live SLA feed.** The public RPC fleet publishes a JSON status object at <https://erpc.datachain.network/v1/fleet-status> that your monitoring can poll. Uptime SLO commitments and paging semantics are documented in section 3.3 of the guide.

Requested pairs, in preference order:
- `FAT/USDT` (primary - CMC data-quality scoring weights this highest)
- `FAT/BTC` (secondary - same reason)
- `FAT/MINTME` (tertiary - cross-listing utility)

Every value in the guide was verified live against production endpoints on 2026-08-14. If your team finds a gap or wants a private high-throughput RPC endpoint for market-making, reply to this email and we will fold the answer back into the same URL within 24 hours.

Happy to jump on a call at your team's convenience.

Thanks,
Adrian Ozinberger
Datachain Foundation SAS
contact@datachain.one
Explorer: <https://dcscan.io>
DEX: <https://dcswap.net>
CMC: <https://coinmarketcap.com/currencies/datachain-foundation/>

---

## 2. XSwap Protocol - Telegram `@xspswap`

Hi XSwap team, this is Adrian from Datachain Foundation. We built Datachain Rope, an EVM-compatible L1 (chainId 271828, native coin FAT) that already runs a Uniswap V2-forked DEX called DCSwap. We would like XSwap to support Rope so XDC-native users can move DC (XRC-20 today, native FAT on Rope after migration) through a familiar front-end.

Two integration options are laid out in an engineering-sized guide we just published:

Option A - route through DCSwap. Zero on-chain deploy on your side; you add one chain entry to your front-end and register the DCSwap Router. Uniswap V2 ABI, unchanged. Full copy-paste wagmi config + init-code-hash + factory / router / pool addresses are in section 8.1.

Option B - deploy XSwap's own DEX contracts on Rope. Only worth it if you want your own venue on our chain (V3 concentrated liquidity, custom fee tiers). Guidance in section 8.2.

Cross-chain flow (XDC XSwap swap output -> legacy DC -> burn -> native FAT on Rope) is documented in section 8.3. Steps 1-4 could be wrapped in a single UX flow once our migration Phase 1 opens.

Guide URL: https://dcscan.io/apis/exchange-integration

Every URL, address and code block was verified live on 2026-08-14 against production endpoints. Reach out on this thread (or email contact@datachain.one) with the option you prefer and we will pair with your engineering to close the loop.

---

## 3. CoinGecko DEX listing - `hello@coingecko.com` (with formal submission at `coingecko.com/en/coins/new`)

**Subject:** New DEX submission - DCSwap on Datachain Rope (chainId 271828)

Hi CoinGecko team,

We are submitting DCSwap - a Uniswap V2-forked DEX on Datachain Rope (chainId `271828`) - for tracking on CoinGecko. The two JSON endpoints your DEX schema requires are already live and CORS-open:

- Pairs list: <https://dcswap.net/api/pairs>
- Tickers: <https://dcswap.net/api/tickers>

A sample `/api/tickers` response with the full field mapping (base + target currency, pool_id, last_price, base_volume, target_volume, liquidity_in_usd, peg tracking) is inline in section 9 of the integration guide we prepared:

**<https://dcscan.io/apis/exchange-integration#coingecko>**

DEX metadata for the submission form:

| Field | Value |
|---|---|
| DEX name | DCSwap |
| DEX website | <https://dcswap.net> |
| Chain | Datachain Rope, chainId 271828 |
| Factory | `0x772e5fd559069aecce5e6983c0c415c8579d780d` |
| Router | `0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4` |
| Wrapped native (WFAT) | `0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4` |
| Init code hash | `0x17abb07a6866e0805d5525f8aa38bfb7e6e0b51cb92df1a1a981d5a96ebdff28` |
| Fork lineage | Uniswap V2 (canonical ABI, verified on-chain) |
| Block explorer | <https://dcscan.io> |
| Public RPC | <https://erpc.datachain.network> |
| Contact | `contact@datachain.one` |

The `/api/pairs` endpoint returns `pair_id`, `base`, `quote` today - the same three fields your DEX spec expects as `ticker_id`, `base`, `target`. If your loader is strict on field names, we can add an aliased `/api/coingecko/pairs` route within an hour of your confirmation - just reply on this thread.

The native FAT coin is already listed on CoinMarketCap as id `10357`. Our supply endpoints (`/api/v1/supply/circulating`, `/api/v1/supply/total`) return CoinGecko-standard bare-text values and are the single source of truth across both trackers.

Thanks,
Adrian Ozinberger
Datachain Foundation SAS
contact@datachain.one

---

## Coordination checklist

Send order (matters for consistency across CMC / CG snapshots):

1. Deploy `dcscan.io/apis/exchange-integration` per publish notes.
2. Verify the URL returns HTTP 200 (see step 5 of publish notes).
3. Send message 1 (MintMe).
4. Send message 2 (XSwap Telegram).
5. Send message 3 (CoinGecko) and file the formal submission at <https://www.coingecko.com/en/coins/new>.

If MintMe requests a private high-limit endpoint before agreeing to list, respond with a scoped API key on `erpc.datachain.network` with a raised `limit_req` zone. That is a rope-vps ops task (edit `deploy/nginx/conf.d/datachain.network.conf`); documented in the existing HA runbooks.

If CoinGecko's DEX loader rejects the `/api/pairs` field names, the aliased `/api/coingecko/pairs` route is a ~30 min shim on the dcswap indexer side. Coordinate with the DCSwap agent.
