# Datachain Rope - Exchange Integration Guide (v1)

**Audience:** exchanges (centralised order-book: MintMe, MEXC, Gate, KuCoin, ...) and decentralised exchange front-ends / aggregators (XSwap Protocol, 1inch, Uniswap Interface forks, Rango, Li.Fi, ...) that want to add Datachain Rope (chainId `271828`, native coin `FAT`) to their supported-network list.
**Date:** 2026-08-14
**Status:** Every API surface documented here is production-live today. No new work is required on the Datachain Rope side to onboard MintMe or XSwap.
**Owner:** Datachain Foundation SAS. Technical contact: `contact@datachain.one`. Ops SLA lives at `https://erpc.datachain.network/v1/fleet-status`.
**Applies to:** Datachain Rope mainnet only (chainId `271828`). No testnet is exposed externally at this time.

---

## 0. TL;DR

Datachain Rope is a fully EVM-compatible L1. You do not need any Rope-specific SDK or bespoke API to integrate. Every call in this document is either:

- **A standard Ethereum JSON-RPC method** served by `https://erpc.datachain.network` (or `https://erpc.rope.network`) - same wire format as Ethereum, same tooling (ethers.js, viem, web3.py, foundry `cast`, go-ethereum client, ...).
- **A CMC/CoinGecko-style public JSON API** served by `https://dcscan.io/api/v1/*` (block explorer + supply reconciliation + token labels).
- **A DCSwap DEX contract** (Uniswap V2 fork) already deployed on-chain, addressable by any Router-aware aggregator.

If your integration works against Ethereum today, adding Rope is a matter of appending one entry to your chains config and (for centralised exchanges) allocating a hot wallet. There is no code to write against a proprietary surface.

---

## 1. Two integration models

The rest of the guide separates by which model applies.

### Model A - Centralised exchange with custody (MintMe, MEXC, ...)

You custody user funds. You need:

1. A way to talk to the chain (JSON-RPC + WSS).
2. Deposit-address generation, funded-tx confirmation, and withdrawal-broadcast paths.
3. A canonical way to represent the asset on your side (name, ticker, decimals, contract if any).
4. Optional: internal risk parameters (finality depth, min withdrawal, fee schedule).

Model A does **not** need to touch DCSwap or any Rope-specific contract. You are trading against your own order book off-chain.

### Model B - DEX / aggregator / DEX front-end (XSwap, 1inch, Rango, ...)

You do **not** custody. You need:

1. A way to talk to the chain (JSON-RPC + WSS) - same as Model A.
2. A canonical liquidity source on Rope. **DCSwap** is a UniswapV2-compatible AMM already deployed; use its Router + Factory directly, or index its pools for quoting.
3. A canonical asset registry (contract address, decimals, logo). See §5.

Model B does **not** need to redeploy any contracts on Rope unless you want your own venue rather than routing through DCSwap.

---

## 2. Network parameters (paste-safe values)

| Field | Value |
|---|---|
| Chain name | `Datachain Rope Mainnet` |
| Chain ID (hex) | `0x425d4` |
| Chain ID (decimal) | `271828` |
| Native coin name | `DC FAT` |
| Native coin symbol | `FAT` |
| Native coin decimals | `18` |
| Consensus | Testimony Consensus (proposer + independent-attester quorum, Reth-based execution, Engine-API driven) |
| Block time (avg) | `~4.2 s` |
| Finality (recommended confirmation depth for exchanges) | `2 blocks` (soft) / `10 blocks` (hard, ~42 s) |
| Public JSON-RPC | `https://erpc.datachain.network` (primary), `https://erpc.rope.network` (secondary) |
| Public WebSocket | `wss://ws.datachain.network` (primary), `wss://ws.rope.network` (secondary) |
| Public block explorer | `https://dcscan.io` |
| Block explorer JSON API | `https://dcscan.io/api/v1/*` |
| Fleet status (uptime SLA) | `https://erpc.datachain.network/v1/fleet-status` |
| Liveness probe | `https://erpc.datachain.network/healthz` |
| HD derivation path | `m/44'/60'/0'/0/0` (BIP-44 Ethereum coin type 60, deliberate for tooling parity) |
| Address format | EIP-55 checksummed (identical to Ethereum) |
| Signature scheme | secp256k1 (identical to Ethereum). Post-quantum hybrid (Ed25519 + Dilithium3) is enforced at consensus but transparent to EVM tooling |
| Genesis date | 2026-03-25 |
| Chainlist | `https://chainlist.org/chain/271828` |

**Self-serve config endpoint (recommended):** every wallet and dApp that integrates Rope should fetch [`https://dcscan.io/api/v1/network/config`](https://dcscan.io/api/v1/network/config) at connect time and push the `eip3085` sub-object to the user's wallet via `wallet_addEthereumChain`. This is how RPC URL rotations are propagated without dApp deploys. **Do not hard-code RPC URLs in your integration** - fetch and cache the config endpoint (5-minute TTL is fine).

Sample response (verified live 2026-08-14):

```json
{
  "eip3085": {
    "chainId": "0x425d4",
    "chainName": "Datachain Rope Mainnet",
    "nativeCurrency": { "name": "DC FAT", "symbol": "FAT", "decimals": 18 },
    "rpcUrls": ["https://erpc.datachain.network", "https://erpc.rope.network"],
    "blockExplorerUrls": ["https://dcscan.io"],
    "iconUrls": ["https://dcscan.io/assets/logo.png", "https://dcscan.io/assets/logo.svg"]
  },
  "chainIdDecimal": 271828,
  "wsUrl": "wss://ws.datachain.network",
  "wsUrls": ["wss://ws.datachain.network", "wss://ws.rope.network"],
  "fleetStatusUrl": "https://erpc.datachain.network/v1/fleet-status",
  "healthzUrl": "https://erpc.datachain.network/healthz",
  "derivationPath": "m/44'/60'/0'/0/0",
  "dcswapUrl": "https://dcswap.net",
  "docsUrl": "https://dcscan.io/apis"
}
```

---

## 3. JSON-RPC surface (standard Ethereum + additive `rope_*`)

Every standard Ethereum method works unchanged. The only additions are `rope_*` methods for Datachain Rope's per-entity ledger primitives (Quipu Canon v1.2). Exchanges and DEXes do not need the `rope_*` methods for basic listing - they are available if you want richer analytics or per-wallet activity streams.

### 3.1 Standard `eth_*` methods (verified compatible)

`eth_chainId`, `eth_blockNumber`, `eth_gasPrice`, `eth_getBalance`, `eth_getTransactionCount`, `eth_getBlockByNumber`, `eth_getBlockByHash`, `eth_getTransactionByHash`, `eth_getTransactionReceipt`, `eth_getLogs`, `eth_sendRawTransaction`, `eth_call`, `eth_estimateGas`, `eth_feeHistory`, `eth_getCode`, `eth_getStorageAt`, `eth_syncing`, `net_version`, `web3_clientVersion`.

WebSocket subscriptions (`eth_subscribe`) supported for: `newHeads`, `logs`, `newPendingTransactions`.

**Ghost-tx guard (exchanges must know this):** `eth_sendRawTransaction` is pinned by the nginx router to the sole active sequencer (BLUE). If your infra ever hits an attester read-endpoint directly and gets an accepted-looking response with no receipt within 30 s, the tx was reclaimed by the autonomous ghost-tx reclaim loop and injected onto the sequencer. In practice, you will never see this if you use `https://erpc.datachain.network` because the router handles it. See `handover-to-dcswap-ghost-tx-reclaim-autonomous-2026-07-29.mdc` for background.

### 3.2 Additive `rope_*` methods (optional, for richer integrations)

| Method | Purpose |
|---|---|
| `rope_globalStats` | Global cord + string + knot counts + per-kind breakdown + invariant flag |
| `rope_listStrings` | Paginated list of registered per-entity strings (wallet, contract, asset, did, cord) |
| `rope_getString` | Descriptor for a single string (genesis knot, head knot, knot count) |
| `rope_knotIndex` / `eth_blockNumber` | Canonical name for cord-anchor knot index (block height alias for EVM tooling) |
| `rope_getKnotByIndex` / `eth_getBlockByNumber` | Same |
| `rope_getKnotByHash` / `eth_getBlockByHash` | Same |

Exchanges typically only need `eth_*`. DEX aggregators may want `rope_globalStats` to display network stats on their chain-picker.

### 3.3 Rate limits, endpoints, and SLA

- Public RPC has no auth key required. Use conservative pacing: peer traffic bursts to ~100 r/s were absorbed on 2026-08-04, but sustained > 200 r/s per client will hit the `limit_req` zone and return HTTP `429`.
- Two RPCs and two WSS URLs are exposed. Round-robin or health-check between them. Behind each URL there is a 4-node failover fleet (BLUE, GREEN, DO-rpc-1, DO-rpc-2) transparent to the client.
- Uptime signal: fetch `https://erpc.datachain.network/v1/fleet-status` every 60 s. Fields to monitor: `writer.status` (`healthy` | `restarting` | `unavailable`), `edge.status`, and `self_heal.escalate_to_cerber` (paging signal - if true, expect degraded write latency for ~180 s).
- Historical outage class: BLUE MTBF ~30 min under peak load (LamportClock contention, documented; a fleet-status flag exists so clients can pre-empt). Adaptive `estimated_recovery_at` and `recommended_deadline_padding_secs` are published in `self_heal` - use them to pad transaction deadlines during a restart window.
- If you need a private high-limit endpoint (bots, indexers, MM engines), request it via `contact@datachain.one`.

---

## 4. Block explorer API (`https://dcscan.io/api/v1/*`)

Standard read-only JSON. All endpoints CORS-open. No auth key required.

### 4.1 Endpoints exchanges typically use

| Endpoint | Returns | Update cadence |
|---|---|---|
| `GET /api/v1/stats` | Network-wide: chainId, latest knot, cord anchors, transactions, events, FAT price, market cap, holders | Realtime (cached 15 s) |
| `GET /api/v1/supply/circulating` | Bare `text/plain` number of circulating FAT (**this is what CMC/CG poll**). Scenario A methodology (uncirculated wallets excluded). | Realtime (5-min recompute) |
| `GET /api/v1/supply/total` | Bare `text/plain` number of total FAT (native emission + migrated legacy DC) | Realtime |
| `GET /api/v1/supply/reconciliation` | Full JSON: total, circulating, per-bucket breakdown, per-wallet uncirculated list, as-of timestamp | Realtime |
| `GET /api/v1/labels` | Canonical `(address -> label)` registry (foundation reserves, DCSwap contracts, Tanastok treasuries, ...) | Cache 5 min |
| `GET /api/v1/accounts/:address/overview` | Balance, tokens, tx count, first-seen, label | Realtime |
| `GET /api/v1/accounts/:address/tokens` | DCR-20 token balances + USD values (via `PriceLens`) | Realtime |
| `GET /api/v1/tokens/:address` | Token detail (name, symbol, decimals, totalSupply, priceUsd, marketCap) | Realtime |
| `GET /api/v1/tokens` | Paginated token summary list | Cache 30 s |

### 4.2 Price feed (canonical)

The single source of truth for FAT / DCR-20 spot price is [`https://dcswap.net/v1/prices`](https://dcswap.net/v1/prices). It publishes:

```json
{
  "data": {
    "FAT": {
      "usd": 0.037089,
      "change_24h": ...,
      "source": "dcswap-reserves(outlier-rejected-gecko)"
    },
    "USDC": { "usd": 0.999967, "source": "coingecko" },
    "USDT": { "usd": 1.0, "source": "coingecko" },
    "EUROD": { "usd": 1.1447, "source": "exchangerate-api" }
  },
  "timestamp": 1786860000,
  "priceMechanism": { "version": "2.1", "phase": "market", ... }
}
```

**Invariant:** WFAT price equals FAT price 1:1 (WFAT is the DCR-20 wrap, same mechanic as WETH:ETH). Any integration that displays WFAT should use `data.FAT.usd`. This invariant is enforced across dcscan by the `PriceLens` diffusion module - see `.cursor/rules/handover-from-dcswap-wfat-price-diffusion-fix-2026-08-13.mdc`.

---

## 5. Asset registry

FAT has three on-chain surfaces, tied by an audited 1:1 burn-and-mint migration. Exchanges list **one** (native FAT); DEXes may need the DCR-20 wrap.

### 5.1 Primary asset

| Field | Value |
|---|---|
| Asset name | `Datachain` (formerly `Datachain Foundation`) |
| Ticker | `FAT` (formerly `DC`) |
| Standard | Native coin of Datachain Rope (chainId `271828`) |
| Contract | None - it is the native gas asset |
| Decimals | `18` |
| Genesis supply | `10,000,000,000 FAT` |
| Max supply | `~18,000,000,000 FAT (asymptotic)` - emitted via anchor-knot rewards under Bitcoin-style 4-year halving. **Not** pre-minted, **not** PoW-mined. See `.cursor/rules/dc-fat-supply-emission.mdc`. |
| Current circulating (2026-08-14) | Poll `https://dcscan.io/api/v1/supply/circulating` |
| Logo | `https://dcscan.io/assets/logo.svg` |
| CMC listing | `id 10357` (`https://coinmarketcap.com/currencies/datachain-foundation/`) |

### 5.2 Wrapped FAT (WFAT) - DCR-20, ERC-20 wire-compatible

Required by any DEX / aggregator that routes swaps against Uniswap-V2-style pools. This is what MetaMask reads for `balanceOf` and what DCSwap LP pairs trade.

| Field | Value |
|---|---|
| Contract | `0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4` |
| Standard | **DCR-20** (Datachain Rope's native fungible token standard, ERC-20 wire-compatible - identical function selectors and events; the naming difference is purely canon, see `.cursor/rules/dcr20-token-standard.mdc`) |
| Name | `Wrapped DC FAT` |
| Symbol | `WFAT` |
| Decimals | `18` |
| Extra methods | `deposit() payable`, `withdraw(uint256)` (same as WETH9) |
| Price | Always equals FAT price 1:1 |

### 5.3 Legacy DC (pre-migration, still tradeable on origin chains until migration completes)

| Chain | Contract | Standard |
|---|---|---|
| Ethereum (chainId 1) | `0x0B44547be0A0Df5dCd5327de8EA73680517c5a54` | ERC-777 |
| XDC Network (chainId 50) | `0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a` | XRC-20 |

These are being **migrated 1:1 to native FAT** via the audited burn-and-mint contracts below. Exchanges should list native FAT, not legacy DC.

### 5.4 Migration contracts (for CMC/CG due diligence packets)

| Contract | Chain | Address |
|---|---|---|
| `EthereumOriginBurn` | Ethereum (1) | `0xAdbE9b023475cf6A150c58F60b65ACB07536e07d` |
| `XdcOriginBurn` | XDC (50) | `0xBf010DAd0C44Ed0481Ed9eDCc01A2DcfD8eE6742` |
| `FATMigrationMinter` | Datachain Rope (271828) | `0x70406ae110D6ccff9a73a2AC2b82d3B666B5a51a` |

Caps: 5,000,000 DC/FAT per tx, 25,000,000 rolling 24h aggregate. See `datachain-rope/docs/DC_FAT_LEGACY_MIGRATION_AND_MARKET_VISIBILITY_SPEC_V2.md` and `.cursor/rules/handover-audit-migration-bridge-2026-07-20.mdc` for the full spec.

### 5.5 Bridged stablecoins (for DEX quote-currency pairs)

| Symbol | Contract | Decimals |
|---|---|---|
| USDC | `0xb93bd8db94f1baff474aa9cba0739daaad01641f` | 6 |
| USDT | `0x79a26132f48394421382c13b54ae77fa3af73289` | 6 |
| EUROD | `0x24d6137807fa8a592888726d87ac748d018c6d4a` | 6 |

All three are DCR-20 (ERC-20 wire-compatible). See `.cursor/rules/handover-dcswap-redeployed-2026-02-26.mdc` for the full DCSwap deployment record.

---

## 6. DCSwap - the canonical DEX on Datachain Rope (Model B primary route)

DCSwap is a Uniswap V2 fork audited and operated by Datachain Foundation. It is the reference liquidity source on chainId 271828. Any aggregator that supports UniswapV2Router02 can integrate DCSwap by adding a new chains entry with these addresses.

### 6.1 Router + Factory

| Contract | Address | Interface |
|---|---|---|
| `DCSwapFactory` | `0x772e5fd559069aecce5e6983c0c415c8579d780d` | UniswapV2Factory ABI (identical) |
| `DCSwapRouter` | `0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4` | UniswapV2Router02 ABI (identical, `WETH()` returns WFAT) |
| `Multicall3` | `0xc2eeb0100aa7e81a3193bdce6733ff767f3bb93a` | Standard Multicall3 |

### 6.2 Live pools (Uniswap V2 pair contracts)

| Pair | Address | Fee | token0 | token1 |
|---|---|---|---|---|
| FAT/USDC | `0xd9ebc3da001618a3ae90481d33ae7ef85e130317` | 0 bps (fee-free) | WFAT | USDC |
| FAT/USDT | `0x644da44bcd5f453c593781dbe22dfd733e8d1441` | 0 bps | WFAT | USDT |
| FAT/EUROD | `0x1e9c2ccf67320459bc4999a9f8be4a063d4021e4` | 0 bps | EUROD | WFAT |
| USDC/USDT | `0xb86bdcecad93573d6ca21313aa7eac52800513c8` | 30 bps | USDC | USDT |

Reserves and TWAP checks are the standard `getReserves()` and `price0CumulativeLast()` / `price1CumulativeLast()` calls. Nothing about the ABI has been changed from UniswapV2 canonical.

### 6.3 Aggregator integration pattern

For an aggregator (1inch / XSwap / Rango / Li.Fi style) to add Rope:

1. Append a new `chains[271828]` entry with the RPC + explorer + native coin as documented in §2.
2. Register the `DCSwapFactory` address in your protocol registry (protocol type: `uniswap_v2`). Your existing UniswapV2 quoting code will work unchanged; the pair init-code-hash used for off-chain CREATE2 pair-address derivation is **`0x17abb07a6866e0805d5525f8aa38bfb7e6e0b51cb92df1a1a981d5a96ebdff28`** (verified live 2026-08-14 via `Factory.pairCodeHash()`, selector `0x9aab9248`). Query it on-chain any time to re-verify - it never changes for a deployed Factory.
3. Register the four pool addresses in your pool registry (or discover them via `Factory.allPairs(i)` / `Factory.allPairsLength()`).
4. Route swaps via `DCSwapRouter.swapExactTokensForTokens`, `swapExactETHForTokens`, `swapExactTokensForETH`, etc. - identical calldata as UniswapV2Router02, `WETH()` returns WFAT.
5. Optional: index Router logs (`Swap`, `Mint`, `Burn`) for realtime pool metrics.

If your aggregator prefers to run its own DEX rather than route through DCSwap: you can deploy any Uniswap V2 / V3 / Solidly / Curve fork onto Rope directly. The chain has no permissioned deploy; Solidity compiler versions up to 0.8.28 are supported. See `datachain-rope/docs/TOKENOMICS.md` for economics context.

---

## 7. MintMe integration playbook (Model A)

MintMe already supports Ethereum, Binance Smart Chain, Solana, BASE, Avalanche, CRO, Arbitrum, Polygon, and MintMe.com Coin. Adding Datachain Rope is mechanically identical to how MintMe added BASE and Arbitrum - it is another EVM chain, no bespoke SDK.

### 7.1 What MintMe engineering needs to do

1. **Chain config entry.** Add one row to MintMe's supported-chains config:
   - `chain_name: "Datachain Rope"`, `chain_id: 271828`, `native_coin: "FAT"`, `decimals: 18`, `explorer: "https://dcscan.io"`.
   - `rpc: "https://erpc.datachain.network"` (with `https://erpc.rope.network` as fallback).
   - `ws: "wss://ws.datachain.network"`.
   - `finality_depth: 10 blocks` (~42 s).
   - `min_confirmations_for_credit: 2 blocks` for user deposits (soft), promote to fully credited at 10.

2. **Wallet infra.** Same as any other EVM chain. Options:
   - **Hot wallet on Rope**: standard secp256k1 keypair, derive using `m/44'/60'/0'/0/N` for N-th user. Fund with FAT for gas from a centralised treasury.
   - **Cold storage**: sign offline; broadcast via `eth_sendRawTransaction` against `https://erpc.datachain.network`. Fine to use hardware wallets - Ledger and Trezor detect Rope as an EVM chain automatically.
   - **Deposit-address generation**: HD-derive as normal. Emit a single Rope address per user - it is EVM-compatible so the address is the same format the user sees in MetaMask.

3. **Deposit detection.** Two paths:
   - **WebSocket `newHeads` + `eth_getBlockByNumber` scan**: subscribe via `wss://ws.datachain.network`, iterate transactions, filter by `to = user_deposit_address`. Recommended for high-throughput CEXes.
   - **Poll `eth_getBalance(address)` on all deposit addresses**: simpler, higher latency, works fine for MintMe's scale.
   - For DCR-20 token deposits (e.g. if MintMe lists USDC/USDT on Rope later): subscribe to `Transfer(from, to, value)` logs on the token contract via `eth_getLogs` with `topics=[0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef, null, "0x0..."+address]`.

4. **Withdrawal broadcast.** Standard `eth_sendRawTransaction`. Two Rope-specific hardenings recommended:
   - **Deadline padding**: read `self_heal.recommended_deadline_padding_secs` from `https://erpc.datachain.network/v1/fleet-status` at broadcast time and add it to your usual deadline. Zero during normal operation; ~180 s during a BLUE restart. This is the same guard DCSwap's own bot uses.
   - **Idempotent submit**: if you get no receipt after 60 s, retry with the same signed tx (same nonce, same hash). The ghost-tx reclaim loop guarantees the tx will land on the sequencer within ~30 s even if it hit an attester's mempool first.

5. **Listing form fields for the FAT market.**

| Field | Value |
|---|---|
| Coin/Token name | `Datachain` |
| Ticker | `FAT` |
| Network | `Datachain Rope` (native coin - request native network support, not a wrapper) |
| Legacy tickers | `DC` on Ethereum (ERC-777) + `DC` on XDC (XRC-20); merged 1:1 via `FATMigrationMinter` |
| Decimals | `18` |
| Circulating supply | Poll `https://dcscan.io/api/v1/supply/circulating` |
| Total supply | Poll `https://dcscan.io/api/v1/supply/total` |
| Max supply | `18,000,000,000 FAT` (asymptotic; emitted via anchor rewards, 4-year halving) |
| Website | `https://datachain.network` |
| Block explorer | `https://dcscan.io` |
| Whitepaper | `https://cdn.prod.website-files.com/64ad7361581c1795281be76e/69fbb91537ccd997078cc500_6e160b5f20d1180954cd44d064c67c4b_Datachain_Rope_Quipu_Proto_Computer_Anthropological_Paper.pdf` |
| Reference DEX | `https://dcswap.net` |
| Contact | `contact@datachain.one` |
| Logo | `https://dcscan.io/assets/logo.svg` |

### 7.2 Recommended trading pairs for MintMe

Per `docs/mintme_cmc_listing_strategy_2026-07-26.md`, in priority order:

1. **`FAT/USDT`** - first choice. CMC and CoinGecko weight pairs against USDT/BTC/ETH highest.
2. **`FAT/BTC`** - second choice, same reason.
3. `FAT/MINTME` - useful as a fallback if the primary quote pair takes longer to set up.

For CMC data-quality scoring, at least one of the first two is required.

### 7.3 Liquidity commitment

Datachain Foundation commits to running a two-sided market-maker on MintMe's FAT/USDT (and FAT/BTC when live), reusing the existing 62-wallet / 9-strategy DCSwap bot architecture redirected at MintMe's order book. Contact: `contact@datachain.one`. A visibly thin or dead order book is worse than no market at all - both CMC and CoinGecko wash-trading filters actively discount markets like that.

---

## 8. XSwap Protocol integration playbook (Model B)

XSwap Protocol (`app.xspswap.finance`) is a Uniswap V2-style DEX front-end on XDC Network. Two integration options exist.

### 8.1 Option A - route swaps through the existing DCSwap DEX (recommended, fastest)

Zero on-chain deployment on XSwap's side. XSwap adds Rope as a supported network in the front-end and routes trades through `DCSwapRouter`.

**Frontend chain config (JavaScript, TypeScript, or wagmi/viem):**

```typescript
export const datachainRope = {
  id: 271828,
  name: 'Datachain Rope',
  network: 'datachain-rope',
  nativeCurrency: { name: 'DC FAT', symbol: 'FAT', decimals: 18 },
  rpcUrls: {
    default: { http: ['https://erpc.datachain.network'], webSocket: ['wss://ws.datachain.network'] },
    public: { http: ['https://erpc.datachain.network', 'https://erpc.rope.network'] },
  },
  blockExplorers: { default: { name: 'DCScan', url: 'https://dcscan.io' } },
  contracts: {
    multicall3: {
      address: '0xc2eeb0100aa7e81a3193bdce6733ff767f3bb93a',
      blockCreated: 1,
    },
  },
} as const;
```

**DEX protocol registration:**

```typescript
export const DCSWAP_ON_ROPE = {
  chainId: 271828,
  routerAddress: '0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4',
  factoryAddress: '0x772e5fd559069aecce5e6983c0c415c8579d780d',
  wrappedNativeAddress: '0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4', // WFAT
  routerAbi: UniswapV2Router02Abi, // unchanged from Ethereum/BSC
  factoryAbi: UniswapV2FactoryAbi,
  // Verified live 2026-08-14 via Factory.pairCodeHash() (selector 0x9aab9248)
  initCodeHash: '0x17abb07a6866e0805d5525f8aa38bfb7e6e0b51cb92df1a1a981d5a96ebdff28',
  defaultQuoteAssets: [
    '0xb93bd8db94f1baff474aa9cba0739daaad01641f', // USDC
    '0x79a26132f48394421382c13b54ae77fa3af73289', // USDT
    '0x24d6137807fa8a592888726d87ac748d018c6d4a', // EUROD
  ],
} as const;
```

Any existing UniswapV2 quoting / swap code (getAmountsOut, swapExactTokensForTokens, addLiquidity, ...) works unchanged.

### 8.2 Option B - deploy XSwap's own DEX contracts on Rope

If XSwap wants its own venue (rather than routing through DCSwap), the deploy path is standard:

1. Deploy XSwap's Factory + Router + WFAT wrapper (or reuse the existing WFAT at `0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4`).
2. Seed pools against WFAT, USDC, USDT, EUROD.
3. Register XSwap's Factory in the DCSwap analytics indexer if you want XSwap pools to appear on `dcscan.io/defi` alongside DCSwap pools (open a PR against `crates/rope-explorer/src/main.rs`, or contact us to add).

Option B is recommended only if XSwap plans to differentiate on features (V3 concentrated liquidity, custom fee tiers, ...). For a straight V2 clone, Option A is faster and lets XSwap inherit DCSwap's existing liquidity.

### 8.3 Cross-chain flow (XDC XSwap <-> Datachain Rope FAT)

If XSwap wants to enable native XDC <-> Rope FAT swaps for users, the recommended route is:

1. XDC-side: user swaps XDC-native or XRC-20 assets on XSwap for **legacy DC (XRC-20)** at `0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a`.
2. XDC-side: user calls `initiateMigration(amount, destinationDatawallet)` on `XdcOriginBurn` at `0xBf010DAd0C44Ed0481Ed9eDCc01A2DcfD8eE6742`. This burns the legacy DC.
3. Rope-side: within ~2 minutes, the migration relayer mints native FAT to the destination address on Rope via `FATMigrationMinter`.
4. Rope-side: user can then swap on DCSwap for WFAT / USDC / USDT / EUROD.

Full spec: `datachain-rope/docs/DC_FAT_LEGACY_MIGRATION_AND_MARKET_VISIBILITY_SPEC_V2.md`. Migration status is polled at `https://dcswap.net/v1/migration/stats` and per-burn tracked at `https://dcswap.net/v1/migration/status/:burnId`.

XSwap could wrap steps 1-4 in a single UX flow ("swap XDC for FAT on Rope") once the migration Phase 1 opens.

---

## 9. CoinGecko DEX listing (DCSwap as tracked DEX on Rope)

This section is written for CoinGecko's integration team and for anyone helping shepherd DCSwap through the CoinGecko new-DEX submission. The goal is for CoinGecko to list DCSwap as a tracked decentralised exchange under Datachain Rope so DCSwap pools (`FAT/USDC`, `FAT/USDT`, `FAT/EUROD`, `USDC/USDT`) start reporting real-time volume + liquidity on `coingecko.com` alongside every other supported DEX.

### 9.1 What's already live (verified 2026-08-14)

DCSwap already exposes the two JSON endpoints CoinGecko's DEX schema requires. No code work on the DCSwap side is required for CoinGecko to start polling.

| CoinGecko required endpoint | Live URL | Status |
|---|---|---|
| Pairs list | `https://dcswap.net/api/pairs` | Live, HTTP 200 |
| Tickers | `https://dcswap.net/api/tickers` | Live, HTTP 200 |
| Historical trades (optional) | not exposed | Optional per CG spec; can be added on request |
| Canonical price feed (auxiliary) | `https://dcswap.net/v1/prices` | Live, HTTP 200 |

Sample response (`/api/tickers`, one ticker abbreviated):

```json
[
  {
    "ticker_id": "FAT_USDC",
    "base_currency": "FAT",
    "target_currency": "USDC",
    "pool_id": "0xd9ebc3da001618a3ae90481d33ae7ef85e130317",
    "last_price": "0.03710279",
    "base_volume": "317133.69942705",
    "target_volume": "11763.46625600",
    "liquidity_in_usd": "3853788.20",
    "peg_expected_price": "1.00000000",
    "peg_deviation_bps": 0,
    "peg_status": "on_peg"
  }
]
```

Sample response (`/api/pairs`, one pair abbreviated):

```json
[
  {
    "pair_id": "0xd9ebc3da001618a3ae90481d33ae7ef85e130317",
    "base": "FAT",
    "quote": "USDC"
  }
]
```

### 9.2 Field mapping (DCSwap live shape -> CoinGecko DEX spec)

| CoinGecko DEX schema field | Source in DCSwap response | Notes |
|---|---|---|
| `ticker_id` | `/api/tickers[].ticker_id` | Present, `{BASE}_{TARGET}` format |
| `base_currency` | `/api/tickers[].base_currency` | Present |
| `target_currency` | `/api/tickers[].target_currency` | Present |
| `last_price` | `/api/tickers[].last_price` | Present, string decimal |
| `base_volume` (24h) | `/api/tickers[].base_volume` | Present, string decimal, sliding 24h |
| `target_volume` (24h) | `/api/tickers[].target_volume` | Present, string decimal, sliding 24h |
| `pool_id` | `/api/tickers[].pool_id` | Present, pair contract address |
| `liquidity_in_usd` | `/api/tickers[].liquidity_in_usd` | Present, string decimal, priced against `PriceLens` snapshot |
| `high` (24h) | not present today | Optional per CG DEX spec; CG typically derives from ticker snapshots |
| `low` (24h) | not present today | Optional per CG DEX spec |
| `bid` | derive `last_price - spread/2` | For a constant-product AMM there is no order book; bid/ask are the marginal price at 1 unit of size |
| `ask` | derive `last_price + spread/2` | Same |
| `product_type` | imply `spot` | DCSwap is spot only, no perps |

For the `/pairs` endpoint, CoinGecko's DEX spec expects `ticker_id`, `base`, `target`, `pool_id`. DCSwap's current `/api/pairs` returns `pair_id`, `base`, `quote` - same information, three field-name differences (`pair_id` -> `ticker_id` + `pool_id`, `quote` -> `target`). CoinGecko has accepted equivalent shapes for other DEXes; if their loader is strict, the DCSwap indexer can add an aliased route at `/api/coingecko/pairs` that renormalizes without changing the canonical endpoint - flag `contact@datachain.one` and this can be shipped in < 1 hour.

### 9.3 Submission steps

1. **Verify endpoints one last time:** confirm `/api/pairs` and `/api/tickers` return HTTP 200 with the sample shape above.
2. **Submit CoinGecko new-DEX request** at `https://www.coingecko.com/en/coins/new` (choose "Add a new DEX" or reach out via `hello@coingecko.com` with the payload below).
3. **Payload for CoinGecko:**

| Field | Value |
|---|---|
| DEX name | `DCSwap` |
| DEX website | `https://dcswap.net` |
| DEX chain | `Datachain Rope` (chainId `271828`) |
| DEX chain CoinGecko slug | (CoinGecko to assign; suggest `datachain-rope`) |
| Pairs endpoint | `https://dcswap.net/api/pairs` |
| Tickers endpoint | `https://dcswap.net/api/tickers` |
| Historical trades endpoint | (not exposed - can be added on request) |
| Factory address | `0x772e5fd559069aecce5e6983c0c415c8579d780d` |
| Router address | `0x8ebdd966e9e9af2ec5d02c886b1c4b5ba617e7c4` |
| Wrapped native (WFAT) | `0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4` |
| Init code hash | `0x17abb07a6866e0805d5525f8aa38bfb7e6e0b51cb92df1a1a981d5a96ebdff28` |
| Fork lineage | Uniswap V2 |
| Block explorer | `https://dcscan.io` |
| Public RPC | `https://erpc.datachain.network` |
| Contact | `contact@datachain.one` |

4. **CoinGecko engineering will:**
   - Poll `/api/pairs` on a slow cadence (typically hourly) to discover pools.
   - Poll `/api/tickers` on a faster cadence (typically every 1-5 min) for live price + volume + liquidity.
   - Backfill and continuously index Router `Swap` logs against `DCSwapRouter` via `https://erpc.datachain.network` for cross-check.
   - Assign a chain slug for Datachain Rope; add DCSwap under it.

5. **Post-listing verification:**
   - DCSwap appears under `https://www.coingecko.com/en/exchanges/decentralized` filtered by the Rope chain.
   - Each pool has a live "Trade" button pointing to `https://dcswap.net`.
   - FAT market cap on `https://www.coingecko.com/en/coins/datachain-foundation` (CoinGecko slug for FAT, once the parent CG listing lands) starts pulling reserve data from DCSwap pools.

### 9.4 Chain slug coordination with CoinMarketCap

The parent-token CMC listing (id `10357`, `https://coinmarketcap.com/currencies/datachain-foundation/`) already tracks native FAT. CoinGecko's DEX listing for DCSwap should reference the same asset registry (§5) so cross-platform price + supply numbers stay in sync. The single source of truth on both is `https://dcscan.io/api/v1/supply/{circulating,total,reconciliation}`.

---

## 10. Acceptance criteria (post-integration smoke tests)

Both platforms should verify the following before go-live.

### 10.1 Chain connectivity

```bash
# Chain ID matches
curl -sS -X POST https://erpc.datachain.network \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","params":[],"id":1}'
# expect: {"jsonrpc":"2.0","result":"0x425d4","id":1}

# Block is advancing
curl -sS -X POST https://erpc.datachain.network \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","params":[],"id":1}'

# Fleet status writer healthy
curl -sS https://erpc.datachain.network/v1/fleet-status | jq '.writer.status'
# expect: "healthy"
```

### 10.2 Native FAT deposit + withdrawal (Model A)

1. Send `0.01 FAT` from Datawallet+ / MetaMask to your generated deposit address.
2. Verify credit within ~10 blocks (~42 s) using `eth_getTransactionReceipt`.
3. Withdraw `0.005 FAT` back to Datawallet+. Verify receipt.
4. Confirm your deposit address's `eth_getTransactionCount` incremented.

### 10.3 DCR-20 (WFAT / USDC) transfer semantics (Model A + B)

1. Read `balanceOf(address)` returns non-zero.
2. Read `Transfer(from, to, value)` log via `eth_getLogs` filtered by the WFAT contract topic.
3. Optional (Model B): quote a `getAmountsOut(1e18, [WFAT, USDC])` on `DCSwapRouter` and verify the result is within 0.5% of `dcswap.net/v1/prices.data.FAT.usd`.

### 10.4 Swap round-trip (Model B)

1. `swapExactETHForTokens(0, [WFAT, USDC], user, deadline) { value: 1e17 }` (~0.1 FAT -> USDC).
2. Verify `Swap` event on the FAT/USDC pool.
3. Verify user's WFAT balance and USDC balance updated.
4. Reverse: `swapExactTokensForETH(usdc_amount, 0, [USDC, WFAT], user, deadline)` after `USDC.approve(router, usdc_amount)`.

---

## 11. Points of contact

| Purpose | Contact |
|---|---|
| Technical integration | `contact@datachain.one` (route to Rope engineering: Kaze Onguene) |
| Listing / market operations | `contact@datachain.one` (route to listing: Adrian Ozinberger) |
| Emergency (RPC down, mempool divergence, unexpected chain state) | Same address; monitor `https://erpc.datachain.network/v1/fleet-status.self_heal.escalate_to_cerber` for autonomous incident detection |
| Public source repo | `https://github.com/KazeONGUENE/rope` |
| Whitepaper | link in §7.1 (MintMe playbook listing form fields) |
| Governance | On-chain via DCSwapTimelock `0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c` (1h min-delay) |

---

## 12. Versioning + change control

This document is versioned in the source tree at `datachain-rope/docs/EXCHANGE_INTEGRATION_GUIDE_v1.md`. Any breaking change (contract redeployment, RPC URL rotation, chain-parameter change) will be:

1. Announced 14 days in advance via the CMC roadmap section and the ecosystem mailing list.
2. Published as a `v2` (or later) revision of this document.
3. Reflected in the live `https://dcscan.io/api/v1/network/config` endpoint at cut-over (which is why we ask integrators to cache-fetch that endpoint rather than hard-code values).

The public JSON-RPC hostnames (`erpc.datachain.network`, `erpc.rope.network`) and the block explorer hostname (`dcscan.io`) are permanent. Backend infra behind them may rotate freely without integrator work.

---

*Every value in this document was verified live against the production endpoints on 2026-08-14. If you spot a drift or find an integration corner case not covered here, please email `contact@datachain.one` and we will fold the answer back into the next revision.*
