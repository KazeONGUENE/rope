# Workstream B - Gated Contract Source Viewer v1

**Author:** Datachain Rope agent
**Date:** 2026-08-12
**Status:** DESIGN. No code shipping in this document. Frames the backlog for adding an Etherscan-style verified-contract source viewer to `dcscan.io/address/:addr` for contract addresses, with an explicit access-control layer that respects the Datachain Foundation's regulatory posture on IP disclosure.
**Prerequisites:** Datachain ID SSO live (`id.datachain.network` per `handover-datachain-id-sso-live-2026-07-07.mdc`); `contracts_registry` in `dc-explorer`; V11 destructive-method gate (`handover-security-audit-2026-06-11.mdc`).

---

## 0. TL;DR

Etherscan / BscScan / XDCScan all show verified contract source code publicly to anyone who visits a contract's address page. This is table-stakes for developer trust: without verification, users can't audit what a contract actually does before signing transactions.

**Datachain Rope's constraint:** many contracts on Rope (Tanastok's ERC-3643 security tokens, DCSwap governance contracts, Datachain Foundation infrastructure like `FATMigrationMinter`) contain compliance logic (KYC gates, transfer restrictions, geographic filtering) that the operators consider **regulated IP** - publishing it publicly could reveal how compliance decisions are made, which is a security risk (an attacker who knows the exact ruleset can craft edge-case exploits) and a competitive risk (rival platforms could clone the logic).

**Workstream B** ships a viewer that supports **three access tiers** for verified contract source:

1. **Public tier** - source visible to anyone visiting `dcscan.io/address/:addr`. Default for open-source contracts (DCSwap Router, Uniswap-lineage code).
2. **Authenticated tier** - source visible only to logged-in Datachain ID users (via `id.datachain.network` SSO). Default for compliance contracts where the operator wants an audit trail of who viewed the source.
3. **Attested tier** - source visible only to users who have posted an on-chain attestation (via `rope_appendToLedger`) accepting an NDA. Default for security tokens and governance code.

All three tiers use the same verification pipeline (Solidity source + compiler settings → deterministic bytecode match) - the difference is only in the access-control gate at read time.

Estimated effort: **10-14 engineering days**. Phase 1 (public tier only) is 5 days. Phases 2-3 (auth + attestation gates) add ~5-9 days.

---

## 1. Why gated (not just public)

The default assumption from Ethereum-mainnet Etherscan is that all contract source should be public. That assumption doesn't survive contact with the Datachain Rope ecosystem for three reasons:

### 1.1 ERC-3643 T-REX security tokens

Tanastok has deployed 411+ organization identities, 415+ asset contracts, and hundreds of ERC-3643 T-REX security-token contracts (per `handover-rope-graph-tanastok-discovery-2026-05-21.mdc`). The T-REX compliance modules encode:

- Country / geography restrictions (which citizens can hold the token)
- Investor accreditation thresholds
- Lock-up periods per investor class
- Trusted-issuer identity verification logic

**Publishing this publicly** would let an adversary read the exact ruleset before attempting to hold the token, which they could game (e.g., "the contract accepts DE-issued IDs but not DE-A-Ost-issued IDs, so I'll get a DE-issued ID"). It would also expose Tanastok's compliance approach to competitors.

### 1.2 DCSwap governance contracts

`DCSwapTimelock` and related governance contracts have known configurations (`minDelay`, `PROPOSER_ROLE`, `CANCELLER_ROLE`) but the tests / edge-case handling around governance transitions are considered operational IP.

### 1.3 Foundation infrastructure

`FATMigrationMinter`, `OriginBurnBase`, `EthereumOriginBurn`, `XdcOriginBurn` (per `handover-from-dcswap-migration-phase0c-2026-07-08.mdc`) implement cross-chain migration cryptography. The design is publishable; the exact hardcoded validator addresses and threshold constants are less so.

**The workstream's job:** support all three access tiers uniformly, and let each contract deployer choose which tier applies to their contract at verification time.

---

## 2. Architecture

```
 Contract owner
 │
 │ (1) POST /api/v1/contracts/verify with source + metadata + tier
 ▼
 ┌──────────────────────────────┐
 │ dc-explorer verify endpoint │
 │ - deterministic recompile │
 │ - bytecode match (creation+ │
 │ deployed) │
 │ - tier assignment │
 │ - store to contracts_registry│
 └──────────────────────────────┘
 │
 │ (2) User visits /address/:contract_addr
 ▼
 ┌──────────────────────────────┐
 │ dc-explorer address handler │
 │ - lookup contract in registry│
 │ - resolve tier │
 │ - check access │
 │ ┌────────────┬──────────┐ │
 │ │ public │authnticd │attstd│
 │ └────────────┴──────────┘ │
 └──────────────────────────────┘
 │
 │ (3) Render tab in SPA with source or access gate
 ▼
 ┌──────────────────────────────┐
 │ address/index.html │
 │ new tab: "Contract Source" │
 │ - public: syntax-highlight │
 │ - auth: "Sign in to view" │
 │ - attested: "Accept NDA to │
 │ view" │
 └──────────────────────────────┘
```

### 2.1 Verification pipeline (all tiers)

Solidity source verification is a well-solved problem. Reuse the Etherscan / Sourcify approach:

1. Contract owner submits: source files, `solc` version, optimizer settings, EVM version, constructor args
2. Server runs the same compiler with the same settings, produces bytecode
3. Compare produced bytecode against on-chain code at the contract address
4. If match, store source in `contracts_registry` with tier assignment
5. Emit `ContractVerified` knot on `governance_ledger_wallet` for audit trail

Rope owns its own compiler infrastructure - use `solc-select` in a sandboxed subprocess (existing pattern in `rope-security::sandbox`). Support solc versions 0.8.0 through latest.

### 2.2 Tier resolution

At contract-page render time:

```rust
enum ContractSourceTier {
 Public, // anyone can view
 Authenticated, // Datachain ID login required
 Attested, // on-chain NDA attestation required
}

async fn resolve_access(
 tier: ContractSourceTier,
 auth_header: Option<&str>, // "Bearer <jwt>"
 attestation_query: Option<&str>, // ?attest=<0x...knot_id>
) -> AccessDecision {
 match tier {
 ContractSourceTier::Public => AccessDecision::Grant,
 ContractSourceTier::Authenticated => {
 verify_datachain_id_jwt(auth_header)
 .map(|_| AccessDecision::Grant)
 .unwrap_or(AccessDecision::RequireAuth)
 }
 ContractSourceTier::Attested => {
 verify_datachain_id_jwt(auth_header)
 .and_then(|user| verify_nda_attestation(user.sub, attestation_query))
 .map(|_| AccessDecision::Grant)
 .unwrap_or(AccessDecision::RequireAttestation)
 }
 }
}
```

### 2.3 Authentication (Tier 2)

Reuse Datachain ID SSO (`https://id.datachain.network` per `handover-datachain-id-sso-live-2026-07-07.mdc`). Frontend flow:

1. User clicks "View source" on an authenticated-tier contract
2. Frontend redirects to `id.datachain.network` for login (email+password or wallet-signature)
3. On success, receives an Ed25519-signed JWT (24h TTL)
4. Frontend calls `GET /api/v1/contracts/:addr/source` with `Authorization: Bearer <jwt>`
5. Backend verifies JWT offline against JWKS, extracts `sub` (user UUID), audits the view

**Audit log:** every source-view for authenticated/attested tiers appends a `ContractSourceViewed` knot to a `dcscan_audit_wallet` (e.g., `0x…d003` following the same pattern as the governance ledger wallet from §Governance handover). This gives contract owners an audit trail if they ever need to prove who viewed their code.

### 2.4 Attestation (Tier 3)

For Tier 3, in addition to Datachain ID login, the user must have posted a signed on-chain attestation accepting an NDA. Attestation shape:

```jsonc
{
 "interaction_type": "ContractSourceNDAAccepted",
 "description": "Accepted NDA for contract source viewing (rope-legal-nda-v1)",
 "metadata": {
 "nda_version": "rope-legal-nda-v1",
 "nda_hash": "0x<sha256 of NDA text>",
 "accepted_at_unix_ms": 1786572000000,
 "user_display_name": "Alice Example"
 }
}
```

User submits this via `rope_appendToLedger` on their own wallet (signed with their wallet key, gated by V11 destructive-methods per Phase-2 EIP-191). The resulting knot ID becomes their "attestation token" that they pass as `?attest=<knot_id>` when requesting source.

**Verification:** backend fetches the knot from the user's personal ledger, verifies:
- The knot's `interaction_type == "ContractSourceNDAAccepted"`
- The knot's wallet matches the JWT's `primary_address`
- The `nda_hash` in metadata matches the current NDA version's hash (embedded in the backend)
- The `accepted_at_unix_ms` is within `NDA_MAX_AGE_SECS` (default 365 days)

If all pass, grant access. Otherwise return 402 (Payment Required semantically = "you owe us an attestation").

---

## 3. Data model

New table in the internal `contracts_registry` (extend existing):

```rust
struct VerifiedContractSource {
 // Existing fields (from current contracts_registry)
 address: [u8; 20],
 name: String,
 symbol: Option<String>,

 // New Workstream B fields
 verified: bool,
 verification_time_unix_ms: u64,
 tier: ContractSourceTier,
 solc_version: String,
 optimizer_enabled: bool,
 optimizer_runs: u32,
 evm_version: String,
 constructor_args_hex: String,
 source_files: Vec<SourceFile>, // { name, content }
 metadata_hash_hex: String, // solc metadata hash
 verifier_address: [u8; 20], // wallet that submitted verification
 verification_tx_hash: [u8; 32], // on-chain ContractVerified knot
}

struct SourceFile {
 name: String, // "MyContract.sol", "IERC20.sol", etc.
 content: String,
}
```

Storage: extend the existing `contracts_registry` RocksDB CF (already keyed by address). Source content compresses well (~10 KB gzipped per typical contract), so ~10 MB for 1000 verified contracts.

---

## 4. API surface

### 4.1 Submit for verification (contract owner)

```http
POST /api/v1/contracts/verify
Content-Type: application/json
X-Rope-Verification-Signature: 0x<sig> (EIP-191 over canonical body)

{
 "contract_address": "0x...",
 "solc_version": "0.8.20+commit.a1b79de6",
 "optimizer": {"enabled": true, "runs": 200},
 "evm_version": "shanghai",
 "constructor_args": "0x...",
 "source_files": {
 "MyContract.sol": "// SPDX-License-Identifier: MIT\npragma solidity 0.8.20;\n...",
 "interfaces/IDCR20.sol": "..."
 },
 "tier": "public" | "authenticated" | "attested"
}

Response 200:
{
 "success": true,
 "verified": true,
 "tier": "public",
 "verification_knot_id": "0x...",
 "note": "Deterministic bytecode match confirmed. Source is now visible to <tier> viewers."
}

Response 400:
{
 "success": false,
 "error": "bytecode_mismatch",
 "expected_hash": "0x...",
 "actual_hash": "0x...",
 "note": "Try enabling/disabling optimizer or check EVM version"
}
```

**Signature:** must be signed by the contract's `owner()` (verified via `eth_call`) or by the deployer address (verified via `eth_getTransactionByHash` on the contract-creation tx). This prevents someone from verifying a contract they don't own.

### 4.2 Read source (public tier)

```http
GET /api/v1/contracts/0x.../source

Response 200:
{
 "success": true,
 "verified": true,
 "tier": "public",
 "solc_version": "0.8.20+commit.a1b79de6",
 "optimizer": {"enabled": true, "runs": 200},
 "source_files": {
 "MyContract.sol": "...",
 "interfaces/IDCR20.sol": "..."
 },
 "verification_knot_id": "0x..."
}
```

### 4.3 Read source (authenticated tier)

```http
GET /api/v1/contracts/0x.../source
Authorization: Bearer <datachain_id_jwt>

Response 200: (same as public)
Response 401: {"success": false, "error": "auth_required", "login_url": "https://id.datachain.network/..."}
```

### 4.4 Read source (attested tier)

```http
GET /api/v1/contracts/0x.../source?attest=0x<attestation_knot_id>
Authorization: Bearer <datachain_id_jwt>

Response 200: (same as public)
Response 402: {"success": false, "error": "attestation_required", "nda_url": "https://dcscan.io/legal/nda-v1", "nda_hash": "0x..."}
Response 401: (if no JWT)
```

### 4.5 List NDA versions (for the SPA)

```http
GET /api/v1/legal/nda-versions

Response 200:
{
 "current": {
 "version": "rope-legal-nda-v1",
 "hash": "0x...",
 "url": "https://dcscan.io/legal/nda-v1",
 "effective_from": "2026-01-01",
 "max_age_days": 365
 },
 "historical": [ ... ]
}
```

---

## 5. Frontend (address SPA changes)

Add a new tab to `address/index.html` (visible only when the address is a contract):

```html
<button type="button" class="addr-tab contract-only" data-tab="source" id="tab-source" style="display:none;">
 <i class="fas fa-code" style="margin-right:6px;"></i>Contract Source
</button>
```

Tab content states:

**State 1: Not verified**
```
This contract has not been verified.
The contract owner can submit source code via the CLI:
 rope-verify submit 0x... --solc 0.8.20 --source MyContract.sol
```

**State 2: Verified, public tier**
Render source files with syntax highlighting (using `highlight.js` or `prism.js`). Include:
- Contract name + symbol
- Compiler version, optimizer settings, EVM version
- All source files as tabs
- ABI + bytecode as separate tabs
- Verification metadata (verifier address, verification date, verification knot ID with link to dcscan.io)

**State 3: Verified, authenticated tier, not logged in**
```
This contract's source is available to signed-in Datachain users.
[Sign in with Datachain ID] button
```

**State 4: Verified, attested tier, no attestation**
```
This contract's source is available to users who have accepted the Datachain NDA v1.
[Read NDA] [Accept and sign attestation] buttons
```

**State 5: Verified, attested tier, attestation valid**
Same rendering as State 2.

### 5.1 Attestation UX flow

Clicking "Accept and sign attestation":

1. Show NDA text in modal
2. User clicks "Accept"
3. Frontend calls `window.ethereum.request({ method: 'wallet_sendTransaction', ... })` to submit a `rope_appendToLedger` tx with the attestation payload
4. Wait for confirmation (~2s at Rope's block time)
5. Extract knot ID from tx receipt
6. Redirect back to source tab with `?attest=<knot_id>` in URL
7. Backend verifies, renders source

---

## 6. Rollout phases

### Phase 1 - Public tier + verification pipeline (5 days)

1. Implement `POST /api/v1/contracts/verify` with deterministic recompile + bytecode match.
2. Extend `contracts_registry` schema with new fields (§3).
3. Implement `GET /api/v1/contracts/:addr/source` (public tier only).
4. Add "Contract Source" tab in SPA with States 1 + 2.
5. Ship a CLI tool `rope-verify submit ...` (thin wrapper around the verify endpoint).

**Acceptance:** DCSwap Router can be verified via the CLI; its source appears publicly on `dcscan.io/address/0x8ebd...`.

### Phase 2 - Authenticated tier + audit log (3 days)

6. Extend verify endpoint to accept `tier` parameter.
7. Extend source endpoint to enforce tier + return 401 if needed.
8. Implement audit log via `dcscan_audit_wallet` knots.
9. Add States 3 + 4 to SPA.
10. Integrate Datachain ID SSO (already live per §handover-datachain-id-sso-live).

**Acceptance:** A `FATMigrationMinter` verified with `tier=authenticated` returns 401 to anonymous requests, source to logged-in users; every view is auditable via `rope_getString(dcscan_audit_wallet)`.

### Phase 3 - Attested tier + NDA flow (5 days)

11. Publish NDA text at `dcscan.io/legal/nda-v1` (legal-team approval required).
12. Implement `verify_nda_attestation` (verify on-chain knot).
13. Extend source endpoint with `?attest=` param.
14. Add State 5 to SPA + attestation modal flow.
15. Ship attestation-signing helper library `rope-nda-attest.js` for other explorers to reuse.

**Acceptance:** A Tanastok T-REX contract verified with `tier=attested` returns 402 to logged-in users without attestation, source to users with a valid on-chain NDA attestation.

### Phase 4 - Polish + tooling (2 days)

16. Bulk-verify tool for Foundation to verify all core contracts in one shot.
17. Extend the `/contracts` list page (existing) to show verification status badge per contract.
18. Add "Re-verify" flow when source is updated (new deployment).

**Acceptance:** all 24 canonical registry contracts (§14 canonical registry) show a "Verified" badge; foundation can bulk-verify from the CLI.

---

## 7. Threat model

### 7.1 Attack: publish false source for a malicious contract

Attacker deploys a benign contract, verifies it publicly, then extracts value via a proxy pattern using the same source hash.

**Mitigation:** verification checks deterministic bytecode match. If the deployed bytecode doesn't match the compiled source (byte-for-byte), verification is rejected. Proxy patterns already surface as `PROXY` in the contracts_registry, and the proxy's implementation is separately verifiable. UI shows proxy status prominently.

### 7.2 Attack: leak source via authenticated tier abuse

An authenticated user views source, then screenshots and publishes it.

**Mitigation:** cannot be prevented technically. Audit log provides accountability - the operator can revoke the user's SSO access and pursue legal action. For high-sensitivity contracts, use the Attested tier (§7.3).

### 7.3 Attack: leak source via attested tier

Same as §7.2 but with a signed NDA. Backend can enforce a legal claim of contract-viewing acceptance.

**Mitigation:** NDA text should be legally binding under Datachain Foundation's jurisdiction (Cameroon / France). Legal team must approve NDA text before Phase 3 ships.

### 7.4 Attack: replay old NDA attestation across multiple contracts

User accepts NDA once, uses the knot ID to view source of every attested contract.

**Mitigation:** by design, the NDA is a one-time acceptance of "I agree to keep source I view confidential." It's not per-contract. The audit log still records every view; operators can revoke access at any time.

### 7.5 Attack: submit fake attestation knot

User forges an attestation knot with a valid `nda_hash` but never actually signed the NDA.

**Mitigation:** knot is signed by the user's wallet (V11 Phase-2 destructive-methods). Forging requires stealing the user's private key. If key is compromised, other risks (asset theft) dwarf this one.

---

## 8. Compliance considerations

### 8.1 GDPR (Art. 17 - erasure)

If a user requests GDPR erasure, their NDA attestation knots must be `rope_untieKnot`'d. The Compliance Agent (`https://compliance-agent.datachain.network`) already handles this pattern. After erasure, the user loses attested-tier access.

### 8.2 Data retention

Audit logs are on-chain knots; they're immutable. Operators can `rope_untieKnot` individual audit entries if legally required (rare - would require a court order).

### 8.3 Regional lockouts

Not implemented in v1. If regulations require blocking source viewers from specific countries, add geo-IP filtering at the nginx layer. Deferred.

---

## 9. Non-goals

- **Automatic decompilation.** Users cannot see decompiled bytecode if the contract owner hasn't verified. This is by design - the operator controls what's shown.
- **Third-party verification (Sourcify-style).** All verification goes through the Rope endpoint. Sourcify integration deferred (compatibility could be added later).
- **Modifying contracts.** This is read-only. Contract upgrades require redeployment + re-verification.
- **Free-form comments / annotations.** Users cannot annotate source. Deferred to a separate "Community" workstream.

---

## 10. Reference

- Datachain ID SSO: `handover-datachain-id-sso-live-2026-07-07.mdc`
- V11 Phase-2 destructive-methods: `handover-security-audit-2026-06-11.mdc`
- Contracts registry: `crates/rope-explorer/src/main.rs::contracts_registry`
- Address SPA: `crates/rope-explorer/static/address/index.html`
- Governance ledger wallet pattern: `handover-governance-voting-platform-phase1-live-2026-07-22.mdc`
- Compliance Agent: `handover-canonical-agents-live-from-rope-2026-05-05.mdc` §compliance-agent
- Related contracts requiring gated tiers:
 - Tanastok ERC-3643 T-REX contracts (from `handover-tanastok-tokenized-assets-for-dcscan-2026-03-30.mdc`)
 - DCSwap governance (Timelock at `0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c` per `handover-from-dcswap-timelock-live-on-271828-2026-06-12.mdc`)
 - FATMigrationMinter, EthereumOriginBurn, XdcOriginBurn (per `handover-from-dcswap-migration-phase0c-2026-07-08.mdc`)

---

*Workstream B is an ~14-day engineering effort split into 4 phases. Phase 1 (public tier) is the highest-priority ship - it brings Rope to Etherscan parity for open-source contracts, unblocking developer trust. Phases 2 + 3 support the Datachain Foundation's regulated-IP posture but require legal sign-off on the NDA text before shipping.*

- Rope agent, 2026-08-12T~14:15Z
