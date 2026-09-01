# Reply to CoinGecko Support - Ticket #136874 (parent #132487)

**Prepared:** 2026-09-01
**To:** Jenz, CoinGecko Support (reply to Zendesk thread; also cc listings@coingecko.com if the reply-to strips)
**From:** Datachain Foundation SAS, contact@datachain.one
**Re:** Canonical logo URLs + clarification of the pending listing request for Datachain Rope (chain), DCSwap (DEX / GeckoTerminal) and DC FAT (native coin)

---

## Draft reply (paste into the Zendesk thread)

Subject: Re: Ticket #132487 - Datachain Rope / FAT canonical logo URLs - specific request + direct URLs

Hi Jenz,

Thank you for the follow-up. To clarify the ask: there is no CoinGecko token page for DC FAT or Datachain Rope yet - that is exactly what parent ticket #132487 is intended to open. The purpose of ticket #136874 is not to edit an existing page, but to hand CoinGecko the canonical brand assets and network parameters so the correct logo is attached the moment the chain, DEX and coin move through your queue.

Specifically, we are asking for three coordinated actions, in the sequencing you originally described to us:

**1. Chain / network registry (CoinGecko platform lookup)**
Please add Datachain Rope as a listed network so it appears in your platform dropdown when contributors submit tokens deployed on it.
- Network name: Datachain Rope
- EIP-155 chain ID: 271828 (`0x425d4`)
- Native currency symbol / decimals: FAT / 18
- Public JSON-RPC: https://erpc.datachain.network (secondary: https://erpc.rope.network)
- Public read-only pool: https://erpc.datachain.network/v1/read
- Block explorer: https://dcscan.io
- EIP-3085 add-to-wallet payload: https://dcscan.io/api/v1/network/config (returns `iconUrls`: PNG first, SVG second, both same-origin)
- Independent listing: https://chainlist.org (network id 271828, entry name "Datachain Rope")
- Logo to attach: `https://dcscan.io/assets/logo.png`

**2. GeckoTerminal - DEX indexing for DCSwap**
DCSwap is the canonical Uniswap-v2-style AMM on chain 271828. Please index it and attach the same brand mark.
- DEX name: DCSwap
- Frontend: https://dcswap.net
- Factory contract: `0x772e5fd559069aecce5e6983c0c415c8579d780d`
- Router contract: `0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4`
- Reference pool (FAT/USDC, zero-fee): `0xd9ebc3da001618a3ae90481d33ae7ef85e130317`
- Canonical price feed: https://dcswap.net/v1/prices (returns `data.FAT.usd` reconciled from DCSwap reserves)
- Logo to attach: `https://dcscan.io/assets/logo.png`

**3. Coin listing - native DC FAT**
The coin submission form is already prepared on our side and, per your prior sequencing, is gated on step 1 (Datachain Rope appearing in the platform lookup). Once the chain is visible we will complete and submit that form. Please attach the same logo asset there when the submission is processed.

**Canonical logo URLs (direct, unwrapped)**
All of the following serve `image/png` or `image/svg+xml`, 256x256 canvas, CORS `Access-Control-Allow-Origin: *`, cache-controlled 1 year, TLS via Let's Encrypt:

- Primary PNG (10,043 bytes): https://dcscan.io/assets/logo.png
- SVG source (98,188 bytes): https://dcscan.io/assets/logo.svg
- Alternate host, same PNG bytes: https://datachain.network/assets/logo.png
- Alternate host, same SVG bytes: https://datachain.network/assets/logo.svg
- IPFS CID (also used by ethereum-lists / ChainList entry "datachain"): `ipfs://bafybeibfkitcey5cpevavib36dv2rxefszbpcxdibyjupizprhuadblccy`
- IPFS via our public gateway: https://ipfs.datachain.network/ipfs/bafybeibfkitcey5cpevavib36dv2rxefszbpcxdibyjupizprhuadblccy
- IPFS via a public gateway (independent verification): https://ipfs.io/ipfs/bafybeibfkitcey5cpevavib36dv2rxefszbpcxdibyjupizprhuadblccy

**One-line independent verification (safe to run)**

```
curl -sSI https://dcscan.io/assets/logo.png
curl -sS  https://dcscan.io/api/v1/network/config | jq .eip3085
```

The `iconUrls` array in that JSON response is what we recommend using as the authoritative source: it is served by the block explorer itself, is signed off by the Datachain Foundation as the network operator, and will remain the single source of truth if the underlying asset is ever re-issued.

Please let us know if you need any additional artifacts (dark-mode variant, favicon, higher resolution, a signed statement from the Foundation attesting to the asset, etc.) or if you would prefer the coin submission form completed and attached to this ticket even while the platform lookup is still pending.

Kind regards,

Datachain Foundation
contact@datachain.one
https://datachain.network

---

## Why this reply (internal notes - do not send)

1. Jenz asked for "the direct URL to the token page where the update should be made." The honest answer is: there isn't one yet. Ticket #132487 is a **new-listing** request, not an update to an existing page. This draft says that plainly in the first paragraph so the ticket doesn't bounce back for the same reason.
2. We give Jenz three concrete actions (chain / GeckoTerminal / coin) so she has an unambiguous set of tasks and can dispatch them to the right internal teams.
3. All URLs are the **direct canonical origins**, not the SendGrid click-tracking wrappers our previous message accidentally exposed. SendGrid wrappers are fragile (they can 404 after N days, they don't preserve CORS, and they surface as "unfamiliar redirector" to CoinGecko's asset-fetcher).
4. `https://dcscan.io/api/v1/network/config` was updated on 2026-08-31 to natively emit PNG-first `iconUrls` from the `dc-explorer` binary (retired the nginx sub_filter hot-patch). Sending Jenz to that endpoint gives her a single machine-readable source of truth she can re-fetch at any point.
5. Included one `chainlist.org` reference so CoinGecko can verify the chain is publicly listed on the industry-standard network directory before adding it to their own.
6. `https://ipfs.io/ipfs/<cid>` is included as an independent-gateway verification path (not our infrastructure) so CoinGecko does not have to trust our pinning setup.
7. The closing paragraph explicitly invites Jenz to request more artifacts or to unblock the coin submission early - both are common CoinGecko workflows and offering them proactively usually shortens the round-trip.

## Verified live at time of drafting (2026-09-01)

- `https://dcscan.io/assets/logo.png` -> HTTP 200, 10043 B, `image/png`
- `https://dcscan.io/assets/logo.svg` -> HTTP 200, 98188 B, `image/svg+xml`
- `https://ipfs.datachain.network/ipfs/bafybe...ccy` -> HTTP 200, 10043 B, `image/png` (same bytes as dcscan.io/assets/logo.png)
- `https://dcscan.io/api/v1/network/config` returns `eip3085.iconUrls = ["https://dcscan.io/assets/logo.png", "https://dcscan.io/assets/logo.svg"]`, `chainId = 0x425d4`, `chainName = "Datachain Rope Mainnet"`

If any of the above regresses before Jenz replies, re-run the verification block in the draft before sending.
