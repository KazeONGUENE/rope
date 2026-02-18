# ComplianceAgent — ERC-3643 Module

> **Author:** Kazé A. ONGUENE — Datachain Foundation  
> **Date:** February 2026  
> **Language:** Rust

---

## Purpose

The `erc3643_module.rs` extends the Datachain Rope ComplianceAgent (one of five AI Testimony Agents) with ERC-3643 awareness. When a T-REX security token transfer triggers a `TestimonyRequested` event on-chain, the ComplianceAgent:

1. Fetches the receiver's ONCHAINID claims from the Datawallet+ ClaimIssuer.
2. Evaluates configurable MiFID II rules off-chain.
3. Writes a `ComplianceDecision` back to the `RopeComplianceModule` contract via `recordExternalTestimony()`.

## Module Location

```
crates/compliance_agent/erc3643_module.rs
```

## Configuration

```rust
ERC3643ComplianceConfig {
    max_holders_per_jurisdiction: HashMap<u16, u64>,
    restricted_countries: HashSet<u16>,
    min_investment_amount: u128,
    require_accredited_investor: bool,
    lockup_period_days: u32,
    compliance_module_address: String,
    identity_registry_address: String,
    rpc_url: String,
}
```

## Validation Rules

| # | Rule | Config Field | Denial Reason |
|---|---|---|---|
| 1 | Minimum investment | `min_investment_amount` | "below minimum" |
| 2 | KYC & AML claims | — (mandatory) | "missing KYC or AML" |
| 3 | Country restriction | `restricted_countries` | "restricted country" |
| 4 | Accredited investor | `require_accredited_investor` | "accredited investor required" |
| 5 | Sovereign identity | — (mandatory) | "sovereign identity required" |

## Testimony Hash

Each decision produces a 32-byte `testimony_hash` computed from `(nonce, allowed, timestamp)`. This hash is stored on-chain in the `RopeComplianceModule` contract's `testimonies` mapping.

## Tests

Run with:

```bash
cd crates/compliance_agent
cargo test
```

Seven test cases cover:
- Valid transfer passes
- Below minimum amount → rejected
- Missing KYC → rejected
- Restricted country → rejected
- Missing sovereign identity → rejected
- Accredited investor required and missing → rejected
- Accredited investor required and present → passes

---

*Kazé A. ONGUENE — Datachain Foundation — February 2026*
