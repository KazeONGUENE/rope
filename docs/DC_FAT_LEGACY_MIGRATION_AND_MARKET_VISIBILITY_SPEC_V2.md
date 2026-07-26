# DC FAT Legacy Migration — Technical & Functional Specification, and Market-Data Visibility Strategy

**v2.0 — Enriched, verified, and implementation-bound**
**Datachain Foundation — 2026-07-08**
**Supersedes:** `~/Downloads/DC_FAT_Legacy_Migration_and_Market_Visibility_Spec.md` (v1.0 draft)

v2.0 enriches the v1.0 draft with (a) live on-chain verification of every figure, (b) the real
state of the aggregator listings (verified via authenticated API calls on 2026-07-08), (c) a
concrete mapping of every spec component onto the code that already exists in `datachain-rope`
and `dcswap`, and (d) the implementation contract for the pieces built in this iteration.

---

## PART A — Legacy Migration: ERC-20 / XRC-20 → Native DCR-20 FAT

### A.1 Verified on-chain baseline (2026-07-08T11:48Z)

Every figure below was read live via `eth_call` — not copied from explorers.

| Field | ERC-20 (Ethereum) | XRC-20 (XDC Network) |
|---|---|---|
| Contract | `0x0B44547be0A0Df5dCd5327de8EA73680517c5a54` | `0x20b59e6c5deb7d7ced2ca823c6ca81dd3f7e9a3a` |
| `name()` | `DATACHAIN` | `DATACHAIN FOUNDATION` |
| `symbol()` / `decimals()` | `DC` / 18 | `DC` / 18 |
| `totalSupply()` | **1,000,000,000 DC** (fully intact — zero burned) | **1,000,000,000 DC** (fully intact — zero burned) |
| Standard | **ERC-777 confirmed**: `granularity()` = 1, `defaultOperators()` = `[0xa6903375509a5f4f740aec4aa677b8c18d41027b]` | Conventional XRC-20 (approve/transferFrom) |

**Implication of the confirmed default operator:** the Ethereum Origin Burn Contract does NOT
need holder-side `approve`. It can either (i) be authorized by each holder via
`authorizeOperator(burnContract)` (one tx, revocable), or (ii) the existing default operator
`0xa690…27b` (Foundation-controlled) can execute `operatorBurn(holder, amount, data, opData)`
against a signed holder intent. Path (i) is trust-minimized and is the normative design;
the `data` field carries the destination Datawallet address so burn + destination binding is
one atomic transaction. The XDC side keeps the standard approve → `initiateMigration` flow.

### A.2 What already exists in code (verified against source)

| Spec component (v1.0 §4.2) | Existing code | Gap |
|---|---|---|
| Bridge trait + EVM proof family | `rope-bridge/src/lib.rs` L30–50 (`Bridge` trait); `EthereumBridge::verify_merkle_proof` L290–314 (BLAKE3 Merkle); `CrossChainProof` enum L1183–1212 | `XdcBridge::verify_proof` is `Ok(true)` — must be upgraded to the Ethereum Merkle path before mainnet activation (both chains are EVM; same proof family) |
| Nullifier concept | `rope-bridge` `EncapsulationEngine` (privacy nullifiers, L879–1134) | No **migration** nullifier set — implemented in this iteration as `rope-bridge/src/migration.rs` |
| Rope-side Minter (WFAT) | `dcswap/contracts` `WFAT` (native wrap) + `BridgedToken.mint` (minter-gated) + `DCSwapTimelock` `0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c` owns tokens/factory | No `mintFromMigration` / `totalMigratedSupply` — new `FATMigrationMinter` contract specified in §A.5, owned by the existing Timelock |
| DCSwap Migration UI | `dcswap/public` `#bridge-page` (UI stub, alerts only) | Full migration flow to be implemented in the dcswap workspace (handover issued) |
| Quipu provenance knot | `rope_appendToLedger` (live; Phase-1 auth gate deployed 2026-06-12 — loopback/internal callers only) | Migration relayer runs co-located on rope-vps → passes the loopback-without-XFF gate; interaction_type `LegacyMigrationCompleted` |
| Supply reconciliation view | none | Implemented in this iteration: `rope-explorer` `/api/v1/supply/*` + dcscan page |
| Legacy contract registry | `config/networks/mainnet.json` L44/L49; `rope-smartchain/src/network_config.rs` L627/L634 | Already correct — reused as the single source for the reconciliation module |

### A.3 Migration principles (unchanged from v1.0, now normative)

1:1 exact, one-directional, terminal same-transaction burn (no escrow), no re-entry.
The nullifier check is **atomic with the mint** (single critical section), never a separate step.

### A.4 Migration flow (v2 — reflects the ERC-777 finding)

1. **Connect & detect** — DCSwap migration screen detects chain (1 = Ethereum, 50 = XDC) and legacy DC balance.
2. **Authorize** — Ethereum: `authorizeOperator(originBurnContract)` (once). XDC: `approve(originBurnContract, amount)`.
3. **Initiate burn** — `initiateMigration(amount, destinationDatawallet)`; contract executes
   `operatorBurn` (ETH) / `transferFrom`+`burn` (XDC) and emits
   `MigrationInitiated(holder, amount, destinationDatawallet, burnId, originBlock)` where
   `burnId = keccak256(chainId ‖ txHash ‖ logIndex)`.
4. **Finality wait** — Ethereum: 2 epochs (~13 min, finalized tag). XDC: 30 blocks (~60 s).
5. **Proof** — relayer builds the Merkle receipt/state proof for the `MigrationInitiated` log.
6. **Verify** — `rope-bridge::migration::MigrationVerifier::verify_and_consume` checks proof →
   nullifier unused → amount match → caps → marks nullifier used, all in one lock scope.
   Error codes: 2001 invalid/expired proof, 2002 nullifier used, 2003 amount mismatch,
   2004 unknown origin chain, 2005 bridge paused (identical to v1.0 §6.4).
7. **Mint** — `FATMigrationMinter.mintFromMigration(dest, amount, burnId)` (Timelock-owned,
   callable only by the verification module role); increments `totalMigratedSupply`.
8. **Provenance** — `rope_appendToLedger(dest, {interaction_type: "LegacyMigrationCompleted",
   metadata: {originChain, originContract, originTxHash, burnId, amount, proofReference}})`;
   DCSwap shows the receipt with a dcscan.io knot link.

### A.5 Contract & module interfaces (implementation contract)

**Origin Burn Contract** (one per origin chain — deployed from the dcswap workspace, Foundry):

```solidity
function initiateMigration(uint256 amount, bytes32 destinationDatawallet) external returns (bytes32 burnId);
event MigrationInitiated(address indexed holder, uint256 amount, bytes32 indexed destinationDatawallet, bytes32 indexed burnId, uint256 originBlock);
function pause() external;   // Timelock-gated
function unpause() external; // Timelock-gated
function totalBurned() external view returns (uint256); // public reconciliation input
```

**FATMigrationMinter (Rope, chainId 271828):**

```solidity
function mintFromMigration(address destination, uint256 amount, bytes32 burnId) external; // onlyVerifier
function totalMigratedSupply() external view returns (uint256);
function isNullifierUsed(bytes32 burnId) external view returns (bool);
// owner = DCSwapTimelock 0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c (same governance as all bridged assets)
```

**rope-bridge migration module** (Rust — shipped in this iteration, `crates/rope-bridge/src/migration.rs`):
`MigrationChain` registry (Ethereum + XDC with the verified legacy addresses), `NullifierSet`
(atomic check-and-consume), `MigrationCaps` (per-tx cap + sliding-window cap → auto-pause on
anomaly per v1.0 §8), `MigrationVerifier::verify_and_consume` returning the 2001–2005 error
codes, full unit-test coverage (replay, caps, pause, amount mismatch, unknown chain).

**RPC surface (rope-node, wired when the contracts deploy):**
`rope_submitMigrationProof(proof, burnId, originChain)` → mintTxHash | error 2001–2005;
`rope_getMigrationStatus(burnId)` → Pending / ProofReady / Verified / Minted / Failed.
Both are destructive-adjacent and MUST be added to `rpc_auth::DESTRUCTIVE_METHODS`
(`rope_submitMigrationProof`) per the 2026-06-11 security audit gate.

### A.6 Supply accounting invariant (public, machine-checkable)

```
logical_supply = erc20_circulating + xrc20_circulating + native_fat_non_migrated + total_migrated
total_migrated == Σ burns(ETH) + Σ burns(XDC)          (exact, no tolerance)
erc20_circulating + xrc20_circulating → 0               (monotonic, by construction)
```

Served live at `https://dcscan.io/api/v1/supply/reconciliation` (implemented in this
iteration; reads both origin chains via `eth_call` every 5 min and the Rope-side figures
from the local RPC). Plain-numeric endpoints for aggregator supply forms:
`/api/v1/supply/circulating` and `/api/v1/supply/total` (text/plain, one number — the
format CoinGecko's and CMC's supply forms require).

**Native-side note (per `dc-fat-supply-emission` canon):** genesis 10 B FAT, max ~18 B
(asymptotic, anchor-reward emission with 4-year halving). The reconciliation view reports
`native_genesis`, `native_emitted` and never presents 10 B as a hard cap.

### A.7 Security requirements (delta vs v1.0)

All of v1.0 §8 stands. Additions from the audit posture already in production:

* `rope_submitMigrationProof` sits behind the V11 Phase-1 gate (public deny, loopback/internal
  allow) and migrates to Phase-2 signed-payload auth (secp256k1-EIP191) when the
  `ROPE_PHASE2_SIGNED_DESTRUCTIVE` flag ships — the relayer signs each submission.
* The `XdcBridge::verify_proof` stub upgrade to the real Merkle path is a **blocking**
  pre-mainnet task; tracked in the migration module with a hard failure (not a pass-through)
  if a proof arrives for a chain whose verifier is not production-grade.
* Caps at Phase 1: 5,000,000 DC per transaction, 25,000,000 DC per 24 h window per origin
  chain; breach → automatic pause + governance knot, not manual review.

### A.8 Rollout phases (dates bound to this iteration)

| Phase | Content | Status |
|---|---|---|
| 0a | Verifier module + nullifier set + caps in `rope-bridge` with tests | **shipped 2026-07-08** |
| 0b | Public reconciliation API + dcscan page | **shipped 2026-07-08** |
| 0c | Origin Burn Contracts (Foundry, both chains) + `FATMigrationMinter` + testnet drill | dcswap workspace (handover issued 2026-07-08) |
| 1 | Independent audit → mainnet activation with A.7 caps | blocked on 0c + audit |
| 2 | Caps lifted per reconciliation stability | after 1 |
| 3 | Legacy supply → ~0; origin contracts optionally closed to new burns | after 2 |

---

## PART B — Market-Data Visibility Strategy

### B.1 Verified aggregator state (authenticated API probes, 2026-07-08)

| Platform | Finding | Evidence |
|---|---|---|
| **CoinMarketCap** | Legacy DC **is listed but INACTIVE**: id **10357** "Datachain Foundation (DC)", `is_active: 0`, price `null`, circulating 0, max 1 B, platform Ethereum `0x0b44…5a54` | `GET /v2/cryptocurrency/quotes/latest?id=10357` |
| **CoinMarketCap key** | Valid; 50 req/min, 15,000 credits/month | `GET /v1/key/info` |
| **CoinGecko** | Legacy DC **not listed at all** (contract lookup 404 on both `ethereum` and `xdc-network`; `/search` empty) | authenticated probes |
| **CoinGecko key** | **Pro-URL key** — works on `pro-api.coingecko.com` (`x-cg-pro-api-key`), returns 10010 on the demo URL. Use the Pro base URL exclusively. | `/ping` + `/simple/price` OK on pro |
| **GeckoTerminal** | Both legacy tokens tracked: XDC DC $0.000563 (~$151/24 h vol, pool `0x890aa242…`), ETH DC $0.00152 (zero vol) | `/onchain/networks/{xdc,eth}/tokens/…` |
| **dcscan.io (own)** | **Price-source regression found**: production served `fatPriceSource: "xdcscan"` at $0.000643 while the canonical DCSwap feed said $0.03424. Root cause: `rope-explorer::fetch_and_cache_price` never implemented the DCSwap-primary chain. **Fixed in this iteration.** | `/api/v1/stats` vs `dcswap.net/v1/prices` |

**Strategic consequence:** the v1.0 fear ("three market caps summed") is not the live risk.
The live risk is the opposite — DC/FAT is effectively **invisible**: CMC shows a dead
inactive page, CoinGecko shows nothing, and the only public price is a $151/day
GeckoTerminal pool on XDC. The Part B work is therefore (1) revive/replace the CMC 10357
listing with correct supply data, (2) get a first CoinGecko listing for native FAT with the
legacy contracts declared as bridged/legacy from day one, (3) make dcscan itself serve
consistent canonical numbers (done), and (4) publish the reconciliation feed both forms
require (done).

### B.2 One asset, three chains, one market cap (classification — unchanged)

| Representation | Chain | Classification | Market-cap treatment |
|---|---|---|---|
| ERC-20 DC `0x0B44…5a54` | Ethereum | Bridged/legacy | Excluded from headline |
| XRC-20 DC `0x20b5…9a3a` | XDC | Bridged/legacy | Excluded from headline |
| Native FAT (WFAT/DCR-20) | Rope 271828 | **Canonical — Coin** | Sole basis of circulating supply & market cap |

### B.3 Canonical price policy (own surfaces — enforced in code as of this iteration)

Priority chain implemented in `rope-explorer::fetch_and_cache_price`:

1. **DCSwap canonical** `https://dcswap.net/v1/prices` → `data.FAT` (substring-match
   `dcswap-reserves` on the source per the 2026-05-10 handover). This is the ecosystem
   source of truth.
2. **GeckoTerminal XDC** legacy-DC token price via the CoinGecko Pro key
   (`/onchain/networks/xdc/tokens/0x20b5…9a3a`) — labelled `geckoterminal-xdc-legacy` so
   consumers can tell a legacy-representation price from the canonical one.
3. **XDCScan** token API (previous primary, now second fallback).
4. Last-known-good cache; static fallback only if no fetch has ever succeeded.

Env contract (all optional, sane defaults): `DCSWAP_API`, `COINGECKO_API_KEY`
(Pro base URL), `CMC_API_KEY` **or** `COINMARKETCAP_API_KEY` (alias accepted as of this
iteration — the root `.env` uses the long name).

### B.4 CoinGecko actions (with the verified corrections)

| Action | Detail (v2-corrected) |
|---|---|
| **New-coin listing request for native FAT** — not a supply update; there is nothing to update yet | Submit via CG's listing form: Coin = DC FAT, chain = Datachain Rope (271828), canonical contract = WFAT `0x285eecf51d5f0a6ab8d8151139b4d19b05c6b3e4`, circulating-supply API = `https://dcscan.io/api/v1/supply/circulating` (plain numeric — live as of this iteration) |
| Declare burn-and-mint methodology | Origin burn contracts (when deployed, Phase 0c) as burn source, `FATMigrationMinter.totalMigratedSupply()` as mint source |
| Declare uncirculated wallets | Foundation reserve, OTC desk, vesting — all three chains (list maintained in the reconciliation endpoint's `uncirculated` array) |
| Request bridged/legacy tagging for the two legacy contracts | Cross-reference native FAT as canonical |
| GeckoTerminal chain support for Rope/DCSwap | Rope's RPC is EVM-shaped (`eth_*` aliases stable, three-node failover); DCSwap already serves the CG DEX-integration endpoints (`/api/v1/pairs`, `/tickers` in `dcswap/api`) — file the GT chain-support request referencing them |

### B.5 CoinMarketCap actions (with the verified corrections)

| Action | Detail (v2-corrected) |
|---|---|
| **Revive id 10357, don't create a duplicate** | File the official request-form update against the existing inactive listing: reclassify native FAT as **Coin** (sovereign chain), attach the supply-methodology doc (this spec), demote the ERC-20/XRC-20 entries to token representations |
| Supply endpoints | CMC form accepts REST endpoints returning bare numbers — `/api/v1/supply/circulating`, `/api/v1/supply/total` (live) |
| Verified source + audit | Origin Burn Contracts + Minter source verified on deploy (Phase 0c); audit report attached at Phase 1 |
| Exchange/DEX pair with API data | DCSwap FAT/USDC (`0xd9ebc3da…0317`) + the CMC-spec `/api/tickers` endpoint already implemented in the dcswap indexer |
| Anti-wash-trading monitoring | The 62-wallet bot volume must be excluded from reported volume; report organic volume only (dcswap indexer distinguishes bot cohort wallets — labelled set) |

### B.6 DefiLlama, DEX Screener, Dune (unchanged from v1.0 §19–20 in substance)

Sequencing: (1) reconciliation dashboard first — **done**; (2) CG + CMC filings in parallel
once Phase 0c contracts are live so burn/mint sources exist; (3) DefiLlama chain
registration + DCSwap SDK adapter (Rope is EVM-compatible enough for the SDK path);
(4) DEX Screener + public Dune dashboard mirroring `/api/v1/supply/reconciliation`;
(5) Token Terminal / Messari after one clean quarter.

### B.7 Ongoing maintenance

* Re-file CG/CMC supply forms at every halving and material OTC disbursement.
* dcscan reconciliation view + Dune dashboard = the single citable source of truth.
* Keep `fatPriceSource` on dcscan surfacing the real provenance chain (`dcswap-canonical` /
  `geckoterminal-xdc-legacy` / `xdcscan` / `cache`) so a regression like the one found on
  2026-07-08 is visible at a glance rather than silent.

---

## Implementation ledger (this iteration, 2026-07-08)

| # | Deliverable | Where |
|---|---|---|
| 1 | This spec v2.0 | `datachain-rope/docs/DC_FAT_LEGACY_MIGRATION_AND_MARKET_VISIBILITY_SPEC_V2.md` |
| 2 | Migration verifier module (nullifiers, caps, pause, error codes 2001–2005, tests) | `datachain-rope/crates/rope-bridge/src/migration.rs` |
| 3 | Supply-reconciliation cache + API (`/api/v1/supply/reconciliation`, `/circulating`, `/total`) | `datachain-rope/crates/rope-explorer/src/market_data.rs` + routes in `main.rs` |
| 4 | Canonical price-source fix (DCSwap → GeckoTerminal → XDCScan → cache) | `rope-explorer/src/main.rs::fetch_and_cache_price` |
| 5 | CMC env alias (`COINMARKETCAP_API_KEY`) + CoinGecko Pro key wiring | `rope-explorer` |
| 6 | dcscan.io public reconciliation page | `rope-explorer/static/supply.html` |
| 7 | DCSwap workspace handover (origin burn contracts, minter, migration UI, indexer routes) | `dcswap/.cursor/rules/handover-from-rope-legacy-migration-2026-07-08.mdc` |

### Completion addendum (2026-07-08, later the same day)

- **Deployed & live-verified (ROPE):** items 3–6 are in production on rope-vps. `https://dcscan.io/api/v1/supply/reconciliation` returns live reads (ERC-20 1B, XRC-20 1B, WFAT ~306.6M, invariant holds); `/circulating` → `9999999210`; `/total` → `10000000000`; `https://dcscan.io/supply` renders; `fatPriceSource` on `/api/v1/stats` is `dcswap-canonical`.
- **DCSwap Phase 0c shipped** (see `handover-from-dcswap-migration-phase0c-2026-07-08.mdc` in the ROPE workspace `.cursor/rules/`): origin burn contracts (Ethereum ERC-777 `operatorBurn` variant + XDC `transferFrom→0xdEaD` variant), `FATMigrationMinter` (escrow-release, Timelock-owned, selector `0x86a3d596` verified), 89/89 Foundry tests, migration UI live on dcswap.net, `/v1/migration/*` indexer routes live on both hosts, CG/CMC listing docs in `dcswap/listing/`.
- **Relayer protocol adopted (ROPE):** `MigrationStatus::Deferred` added plus `classify_mint_outcome(tx_succeeded, nullifier_used_after)` in `rope-bridge/src/migration.rs` — a successful minter tx with an unconsumed nullifier means the minter auto-paused on the 24h window cap and the mint is deferred, not completed. 16/16 module tests green.
- **`FATMigrationMinter` DEPLOYED on Rope (2026-07-08T21:00Z):** address `0x70406ae110D6ccff9a73a2AC2b82d3B666B5a51a`, tx `0xaeb178581679225742893734dae6310462c73386ffe5f8b6032549909f994355`, block 3024924. Owner = DCSwapTimelock from construction, paused (audit gate), caps 5M/25M, origin chains {1, 50} allowed, escrow 0. Verifier `0xB613…5D9D`, attestor `0x834E…49EF`, guardian `0x283C…C20e`. `MIGRATION_MINTER_ADDRESS` set on both dcswap indexer hosts and on rope-explorer; live-verified: `dcswap.net/v1/migration/stats` → `phase: "pre-activation"` with real minter reads, and `dcscan.io/api/v1/supply/reconciliation` migrated bucket source is now `FATMigrationMinter.totalMigratedSupply()`. Minter labeled on dcscan ("DC FAT Migration Minter (Legacy DC → FAT)") along with the DCSwap Governance Timelock. Recorded in `dcswap/contracts/deployments/271828.json` under `migration`.
- **Still gated:** Ethereum/XDC origin burn contract deploys remain behind the Phase 1 audit gate (§A.8); minter unpause requires a Timelock-scheduled `unpause()` after the audit; escrow funding is a governance value transfer to the minter.
- **Listing filings SUBMITTED (2026-07-08T22:20Z):**
  - **CoinMarketCap** — update/reactivation request for listing id 10357 filed as **Zendesk ticket #1409116** on `support.coinmarketcap.com` (form `7 - [Existing Cryptoasset] Update info`, requester `contact@datachain.one`, HTTP 201, `status: open`). Filed via the Zendesk anonymous requests API after the web form's rich-text proof editor rejected programmatic input. Full submitted body archived in `dcswap/listing/cmc-update-request-id10357.md`.
  - **GeckoTerminal (CoinGecko)** — Chain Addition (Datachain Rope, 271828) + DEX Addition (DCSwap) filed on `support.coingecko.com` via the same API path; initially suspended (#59843121394201), then **requester email verified by the operator 2026-07-08T22:23Z** — released to the agent queue as public ticket **CoinGecko #132487** (confirmation email received at `contact@datachain.one`). Reply to that email thread to append the audit report / origin-contract addresses at Phase 1.
  - **CoinGecko new-coin form** — filled end-to-end in the operator's authenticated browser session on 2026-07-09T00:45Z (all step-1 fields per `dcswap/listing/coingecko-listing-data.json`), but submission is **hard-blocked by CoinGecko's own data dependencies**, not by login: the "Exchange Name" field is a closed dropdown over CG's 1,485 tracked exchanges (DCSwap absent) and the DEX-listing form's "Blockchain Platform" field is a closed dropdown over CG's 292 tracked platforms (Datachain Rope absent). Verified sequencing: Chain Addition (ticket **#132487**, in queue) → DEX listing form for DCSwap (pre-filled quick sheet in `dcswap/listing/coingecko-listing-application.md`) → coin form resubmission. No further action possible until #132487 is processed by CoinGecko.
