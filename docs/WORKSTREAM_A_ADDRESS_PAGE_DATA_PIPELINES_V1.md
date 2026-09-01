# Workstream A - Address-Page per-Tab Data Pipelines v1

**Author:** Datachain Rope agent
**Date:** 2026-08-12
**Status:** DESIGN. No code shipping in this document. Frames the work backlog for turning `dcscan.io/address/:addr` into an Etherscan-parity address-detail experience backed by the `rope-addr-index` (§Session N+3 wired for Transactions + Token Transfers).
**Prerequisites:** `rope-addr-indexer` service deployed on rope-vps (staged per `docs/ROPE_ADDR_INDEX_RUNBOOK.md`, operator-gated); `dc-explorer` reader path wired (already landed in `main.rs::account_transactions_inner` + `extra.rs::account_transfers`).

---

## 0. TL;DR

The address-detail SPA at `crates/rope-explorer/static/address/index.html` already has 8 tabs surfaced in the UI:

1. String & Knots (personal-ledger native path)
2. Transactions (Reth-anchored, now backed by rope-addr-index)
3. Internal Txns (empty today - not indexed)
4. Token Transfers (DCR-20) (Reth logs scan, now backed by rope-addr-index)
5. NFT Transfers (ERC-721) (rendered only for token contracts)
6. Holders (rendered only for token contracts)
7. Inventory (rendered only for NFT contracts)
8. Owners (rendered only for NFT contracts)

**Every tab shares the same underlying pattern:** the frontend fires a `fetch('/api/v1/accounts/:addr/<endpoint>')` on tab-click, expects `{success: true, source: "native|reth-anchored|reader|cache|unavailable", items: [...], pagination: {...}}`, and renders. **What breaks Etherscan-parity today** is that half these endpoints don't exist and the ones that do have inconsistent shapes, no pagination, and no cross-tab consistency guarantees.

**Workstream A** fixes this by defining a single canonical pipeline shape for every tab, wiring each pipeline through the appropriate data source (rope-addr-index for chain reads, contracts_registry for token metadata, personal-ledger native for knots), and adding the missing tab endpoints (`internal`, `nft-transfers`, `holders`, `inventory`, `nft-holders`).

Estimated effort: **12-16 engineering days** spread across 5 phases. Phase 1 (canonical pipeline shape + 2 tabs migrated) is 3 days. Full completion + tests + docs is 4 weeks calendar time.

---

## 1. Current state audit (2026-08-12)

For each of the 8 tabs, this table records: what endpoint the frontend calls, whether that endpoint exists, and what data source it uses.

| Tab | Frontend URL | Backend handler | Source | Status |
|---|---|---|---|---|
| String & Knots | `/api/v1/personal-ledger/:addr/string?limit=50` | `main.rs::personal_ledger_string` | Native ledger via `rope_getStringWithKnots` + Reth fallback | LIVE (with SWR + timeout guard from §16, §Session N+2) |
| Transactions | `/api/v1/accounts/:addr/transactions?page=1&limit=25` | `main.rs::account_transactions` | rope-addr-index (Session N+3) with Reth fallback | LIVE (reader-first path just added) |
| Internal Txns | (no fetch wired) | N/A | rope-addr-index `addr_internal` CF (unpopulated) | NOT-STARTED |
| Token Transfers (DCR-20) | `/api/v1/accounts/:addr/transfers?page=1&limit=25` | `extra.rs::account_transfers` | rope-addr-index (Session N+3) with `KNOWN_DCR20_ADDRS` filter | LIVE (reader-first path just added) |
| NFT Transfers (ERC-721) | (no fetch wired) | N/A | rope-addr-index (needs ERC-721 Transfer topic0 filter) | NOT-STARTED |
| Holders | `/api/v1/tokens/:addr/holders` (?) | Not wired | rope-addr-index reverse scan of Transfer events | NOT-STARTED |
| Inventory | (no fetch wired) | N/A | rope-addr-index NFT ownership walk | NOT-STARTED |
| Owners | (no fetch wired) | N/A | rope-addr-index NFT ownership walk | NOT-STARTED |

**Cross-tab consistency issue:** each existing tab's response shape is subtly different. `personal_ledger_string` returns `{knot_count, knots[], source, note?}`. `account_transactions` returns `{transactions[], source, note?}`. `account_transfers` returns `{transfers[], source, note?}`. No two tabs have the same pagination shape, so the frontend has 3 separate rendering paths.

---

## 2. Canonical pipeline contract

Every address-page tab endpoint MUST conform to this shape:

```jsonc
{
 "success": true,
 "endpoint": "transactions", // matches tab name for round-trip debugging
 "source": "reader" | "native" | "reth-anchored" | "cache" | "cache-warming" | "unavailable",
 "items": [ /* tab-specific object shape */ ],
 "pagination": {
 "page": 1,
 "limit": 25,
 "total_estimated": 12483, // best-effort count (may be capped)
 "has_next": true,
 "next_cursor": "0x00000000387234a1_00000012" // opaque; frontend passes back as ?cursor=
 },
 "note": "optional human-readable degradation reason"
}
```

**Cursor semantics:** cursor is opaque to the frontend. Backend chooses format based on data source:
- rope-addr-index: `hex(block_be, 8bytes) || _ || hex(idx_be, 4bytes)` (already implemented)
- personal-ledger: `hex(knot_index, 8bytes)` (chain walk)
- Reth fallback: `block_number:tx_index` (integer pair)

Frontend never parses cursor; it treats it as an opaque handle to fetch the next page.

**Timeouts and fallbacks (SWR pattern from §Session N+1):**
- Every endpoint wraps its compute path in `tokio::time::timeout(Duration::from_secs(N))` where N is 8s for reader-served endpoints, 20s for RPC-fanout endpoints
- On timeout, return `{success: true, source: "unavailable", items: [], pagination: {...}, note: "..."}` with HTTP 200, NEVER a 504

**Consistency guarantees:**
- All tab endpoints on the same address MUST use the same `head_block` snapshot when possible (pass `?block=<hex>` for reproducibility)
- If two tabs return `head_block=X` and `head_block=Y` where Y > X, the frontend can show a "chain advanced" indicator - not an error

---

## 3. Per-tab pipeline design

### 3.1 Tab: String & Knots (LIVE, minor cleanup)

**Endpoint:** `GET /api/v1/personal-ledger/:addr/string?limit=50&cursor=...`

**Source:** Native personal-ledger via `rope_getStringWithKnots` when the address has a native ledger. Falls back to `rope-addr-index` for the tx-anchored view when no native ledger exists. Falls back to Reth-scan (§9 §16 timeout guard) when neither is available.

**Item shape:**
```jsonc
{
 "knot_id": "0x...",
 "sequence_number": 0,
 "event_type": "AttestationAnchored" | "TransferReceived" | ...,
 "content_ref": "0x...",
 "authorizer": "0x...",
 "chain_tx": {"hash": "0x...", "block_hex": "0x..."} // optional Reth cross-ref
}
```

**Cleanup ask:** move existing handler to conform to §2 shape (add `endpoint`, unify `items` field name).

### 3.2 Tab: Transactions (LIVE, cleanup + count)

**Endpoint:** `GET /api/v1/accounts/:addr/transactions?page=1&limit=25&cursor=...`

**Source:** rope-addr-index reader (`AddressIndex::transactions`) is the primary. Fallback to `collect_txs_from_recent_blocks` with 8s timeout.

**Item shape:**
```jsonc
{
 "hash": "0x...",
 "block_number": 3849287,
 "block_hex": "0x3aa1b7",
 "tx_index": 12,
 "timestamp": "2026-08-12T12:34:56Z",
 "from": "0x...",
 "to": "0x..." | null,
 "value_wei": "0x...",
 "value_fat": "1.234500000000000000",
 "gas_used": 21000,
 "gas_price_wei": "0x3b9aca00",
 "status": "0x1",
 "method_id": "0xa9059cbb" | null,
 "method_name": "transfer(address,uint256)" | null,
 "direction": "in" | "out" | "self"
}
```

**Cleanup ask:** add `pagination.total_estimated` from `AddressIndex::count_transactions_bounded` (already implemented but not surfaced).

### 3.3 Tab: Internal Txns (NOT-STARTED)

**Endpoint:** `GET /api/v1/accounts/:addr/internal?page=1&limit=25&cursor=...`

**Source:** rope-addr-index `addr_internal` column family. **Population:** the CF exists in schema (§Session N+2 rope-addr-index staged) but the indexer's `writer.rs` currently populates only `addr_tx` and `addr_log` because internal-tx extraction requires `debug_traceTransaction` RPC calls (expensive - ~500ms/block on rope-vps).

**Rollout options:**
- (A) **Populate `addr_internal` at ingest time.** Cost: adds ~500ms per block to indexer. Acceptable if the indexer runs on its own tokio task and doesn't block RPC.
- (B) **Lazy populate on tab-click.** First user to open the Internal Txns tab for a given address triggers a `debug_traceTransaction` for each of that address's recent txs, results cached in RocksDB. Cheap in steady state; slow first click.
- (C) **Skip for now.** Show `<empty>` state until the indexer is upgraded.

**Recommendation:** Ship (C) initially with `{source: "unavailable", note: "Internal transactions not indexed yet - Phase 3 target"}`. Move to (A) once P2B parallel writers land (§22) and give indexer ~50% headroom.

**Item shape** (when populated):
```jsonc
{
 "parent_tx_hash": "0x...",
 "parent_tx_index": 12,
 "block_number": 3849287,
 "trace_address": [0, 1], // path in the call tree
 "call_type": "CALL" | "DELEGATECALL" | "STATICCALL" | "CREATE",
 "from": "0x...",
 "to": "0x...",
 "value_wei": "0x0",
 "gas_used": 21000,
 "input": "0x...",
 "output": "0x..." | null,
 "error": "revert" | null
}
```

### 3.4 Tab: Token Transfers (DCR-20) (LIVE, cleanup)

**Endpoint:** `GET /api/v1/accounts/:addr/transfers?page=1&limit=25&cursor=...`

**Source:** rope-addr-index reader (`AddressIndex::logs` filtered by `TRANSFER_TOPIC`) with `KNOWN_DCR20_ADDRS` filter for token contract allowlist. Fallback to `eth_getLogs` recent-blocks scan.

**Cleanup ask:** Extend `KNOWN_DCR20_ADDRS` from static allowlist to dynamic - query `contracts_registry` (existing) for verified DCR-20 tokens. This way new tokens deployed on Rope automatically appear in the transfers tab without a code change.

### 3.5 Tab: NFT Transfers (ERC-721) (NOT-STARTED)

**Endpoint:** `GET /api/v1/accounts/:addr/nft-transfers?page=1&limit=25&cursor=...`

**Source:** rope-addr-index reader. Same `TRANSFER_TOPIC` topic0 as DCR-20, but the topic-count of the log distinguishes: `Transfer(address indexed from, address indexed to, uint256 indexed tokenId)` has 4 topics (topic0 + 3 indexed); `Transfer(address indexed from, address indexed to, uint256 value)` has 3 topics (topic0 + 2 indexed). Filter on topic count == 4 to identify ERC-721.

**Item shape:**
```jsonc
{
 "hash": "0x...",
 "block_number": 3849287,
 "block_hex": "0x3aa1b7",
 "log_index": 5,
 "timestamp": "2026-08-12T12:34:56Z",
 "contract_address": "0x...",
 "contract_name": "Tanastok NFT Deed" | null,
 "contract_symbol": "TNFT" | null,
 "from": "0x...",
 "to": "0x...",
 "token_id": "0x2a",
 "token_id_decimal": "42",
 "direction": "in" | "out" | "self",
 "metadata_uri": "https://..." | null // resolved from tokenURI() if cheap
}
```

**Rollout:** Ship without `contract_name`/`contract_symbol`/`metadata_uri` initially (source: "reader-basic"), then enhance with `tokenURI()` resolution and IPFS pinning integration in Phase 2.

### 3.6 Tab: Holders (token contracts only) (NOT-STARTED)

**Endpoint:** `GET /api/v1/tokens/:contract/holders?page=1&limit=25&cursor=...`

**Source:** Aggregation over rope-addr-index Transfer events. **NOT** a simple balance snapshot - requires computing net balance per address by summing all inbound minus outbound Transfer events.

**Two computation strategies:**
- (A) **On-demand:** stream all Transfer events for the contract, aggregate in-memory. Cost: O(all_transfers). For contracts with 10k+ transfers, this exceeds the 8s timeout.
- (B) **Precomputed:** on every ingested block, update a `holder_balances` sub-index. Cost: 3 RocksDB puts per Transfer event (delete old, insert new, invalidate cache). Storage cost negligible.

**Recommendation:** Implement (B) as a new column family `addr_holder{contract}` keyed by `(contract, holder_address)` with value `{balance_hex, last_updated_block}`. Only materialise for token contracts (skip for arbitrary EOAs). Add during Phase 3 of Workstream A.

**Item shape:**
```jsonc
{
 "rank": 1,
 "holder_address": "0x...",
 "holder_label": "DCSwap Router" | null,
 "balance_wei": "0x...",
 "balance_formatted": "12345.678900",
 "percentage_of_supply": "0.234",
 "first_transfer_block": 1234567,
 "last_transfer_block": 3849287,
 "transfer_count": 42
}
```

### 3.7 Tab: Inventory (NFT contracts only) (NOT-STARTED)

**Endpoint:** `GET /api/v1/accounts/:addr/nft-inventory?contract=0x...&page=1&limit=25&cursor=...`

**Source:** rope-addr-index NFT ownership walk. For each `token_id` that appears in `Transfer` events for the given contract, take the most recent Transfer's `to` field as the current owner. Filter to the queried address.

**Rollout:** Requires the `holder_balances` sub-index from §3.6 to be extended with per-`token_id` tracking. Group in Phase 3.

**Item shape:**
```jsonc
{
 "contract_address": "0x...",
 "contract_name": "Tanastok NFT Deed",
 "contract_symbol": "TNFT",
 "token_id": "0x2a",
 "token_id_decimal": "42",
 "metadata_uri": "https://..." | null,
 "image_url": "https://..." | null,
 "name": "Kibali Gold Mine #42" | null,
 "acquired_block": 3849287,
 "acquired_tx_hash": "0x..."
}
```

### 3.8 Tab: Owners (NFT contracts only) (NOT-STARTED)

**Endpoint:** `GET /api/v1/tokens/:contract/nft-owners?page=1&limit=25&cursor=...`

Same shape as Holders (§3.6) but keyed per-`token_id`. Requires same sub-index.

---

## 4. Rollout phases

### Phase 1 - Canonical pipeline shape (3 days)

1. Refactor `personal_ledger_string`, `account_transactions`, `account_transfers` to emit the canonical §2 shape.
2. Rename inconsistent field names (`knots` → `items`, `transactions` → `items`, `transfers` → `items`) with legacy aliases preserved for one release.
3. Add `pagination` object to all three (currently absent from `account_transactions`).
4. Add `endpoint` field for round-trip debugging.
5. Frontend: unify the 3 rendering paths into one function.

**Acceptance:** all 3 existing tabs render with identical rendering code paths; no visual regressions; response bodies conform to §2 shape.

### Phase 2 - NFT Transfers (2 days)

6. Extend `AddressIndex::logs` iterator with a topic-count filter (or add a `Transfer` topic-count field to `LogRef` at ingest).
7. Add `GET /api/v1/accounts/:addr/nft-transfers` handler.
8. Wire NFT tab in the frontend (currently `display: none`).
9. Update `contracts_registry` to include ERC-721 contract metadata.

**Acceptance:** clicking NFT Transfers tab on a Tanastok NFT deed contract shows the correct in/out list; empty state renders cleanly.

### Phase 3 - Holder + Inventory sub-index (5 days)

10. Design + implement `addr_holder{contract}` CF in rope-addr-index schema.
11. Extend indexer `writer.rs` to update the CF on every Transfer event.
12. Add `GET /api/v1/tokens/:contract/holders`, `/api/v1/accounts/:addr/nft-inventory`, `/api/v1/tokens/:contract/nft-owners`.
13. Wire Holders / Inventory / Owners tabs in the frontend.

**Acceptance:** DCSwap USDC contract shows top 100 holders sorted by balance; a wallet with 3 Tanastok NFT deeds shows all 3 in Inventory tab.

### Phase 4 - Internal Txns (3 days)

14. Extend indexer with `debug_traceTransaction` on ingest (or lazy on-demand per §3.3).
15. Populate `addr_internal` CF.
16. Add `GET /api/v1/accounts/:addr/internal` handler.
17. Wire Internal Txns tab.

**Acceptance:** clicking Internal Txns on a contract that had CALLs from another contract shows the internal call tree.

### Phase 5 - Polish (3 days)

18. Add `?block=<hex>` reproducibility param to all endpoints.
19. Wire cross-tab consistency indicator ("chain advanced during your session").
20. Extend `KNOWN_DCR20_ADDRS` to query `contracts_registry` dynamically.
21. Add integration test suite covering all 8 tabs against a rope-vps mirror.

**Acceptance:** every tab loads in < 500ms p99 for the top-100 most-active addresses on Rope (DCSwap Router, Tanastok Treasury, etc.); no tab returns 504 under normal load.

---

## 5. Storage cost analysis

Rough numbers (based on rope-vps disk usage 2026-08-12):

- rope-addr-index today: `addr_tx` + `addr_log` = ~4 GB for 3.85M blocks = ~1 KB per block avg
- Adding `addr_internal`: est +30% ≈ ~5.5 GB total. Fits in 40 GB rope-vps disk.
- Adding `addr_holder{contract}`: negligible for now (~50 tokens × ~1000 holders each × 32 bytes = 1.6 MB). Grows linearly with token count.
- Adding NFT-per-token ownership: negligible until Tanastok mints > 10k NFTs. Even at 1M NFTs, ~32 MB.

**No hardware changes needed** for Workstream A end-state on current rope-vps.

---

## 6. Dependencies on other workstreams

- **P2B (§22):** parallel-writer backend eases the write-path pressure and gives indexer headroom to add `debug_traceTransaction` calls. Recommended to complete P2B soak before Phase 4.
- **Grafana (Workstream Observability):** dashboards for `dc_explorer_addr_index_reader_hits_total` help validate that reader-first paths are being hit. Recommended parallel with Phase 1.
- **rope-addr-indexer deploy (Session N+2 staged):** must be running on rope-vps before ANY reader-first path serves real traffic. Currently staged, ops-gated.

---

## 7. Non-goals (explicitly out of scope)

- **Historical token price at time of transfer.** Requires a price oracle at every block. Deferred to a separate "Analytics" workstream.
- **Gas fee analytics per address.** Same rationale.
- **Multi-chain unified view.** Cross-Etherscan / cross-XDCScan sibling links exist (§16 §address-page sibling bar) but aggregated balance across chains is separate scope.
- **Transaction annotation / notes.** No user-writable data on chain reads. Would require a separate labels service.
- **Contract source code viewer.** That's Workstream B.

---

## 8. Reference

- Existing address SPA: `crates/rope-explorer/static/address/index.html`
- Existing handlers: `main.rs::account_transactions_inner`, `main.rs::personal_ledger_string`, `extra.rs::account_transfers`, `extra.rs::account_events`
- Session N+3 wire-up: `main.rs::try_account_transactions_from_index`, `extra.rs::try_account_transfers_from_index`
- rope-addr-index: `crates/rope-addr-index/src/reader.rs`, `crates/rope-addr-index/src/schema.rs`
- Contracts registry: `crates/rope-explorer/src/main.rs::contracts_registry` (used by DCR-20 filter)
- Related handovers:
 - `handover-from-dcswap-dcscan-address-parity-fixes-2026-08-11.mdc` §14, §16
 - `ROPE_ADDR_INDEX_RUNBOOK.md` (indexer deploy)

---

*Workstream A is an ~16-day engineering effort split into 5 phases. Phase 1 (canonical shape + 2 tabs migrated) is the highest ROI (unifies 3 rendering paths, closes cross-tab consistency risk) and unblocks all subsequent phases. Everything after Phase 1 is additive and can slip without regressing the current LIVE state.*

- Rope agent, 2026-08-12T~14:00Z
