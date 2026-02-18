# ONCHAINID / ERC-3643 Integration — Datachain Rope

> **Author:** Kazé A. ONGUENE — Datachain Foundation  
> **Date:** February 2026  
> **Status:** Implementation v1.0

---

## Overview

This document describes the integration of the **ONCHAINID** protocol (ERC-734 / ERC-735) and the **ERC-3643** (T-REX) standard into Datachain Rope, enabling compliant security token issuance with Datawallet+ as the native identity provider.

## Architecture

```
Datawallet+ (React Native)
    │
    ├─ ClaimIssuer.ts ──► signs & issues claims to ONCHAINID
    ├─ eligibility.ts ──► checks ERC-3643 token eligibility
    └─ OnchainIDView.tsx ─► UI for claims & key management
         │
         ▼
Datachain Rope (Chain ID: 271828)
    │
    ├─ IdFactory (CREATE2) ──► deploys ONCHAINID proxies
    ├─ DatawalletClaimIssuer.sol ──► trusted claim issuer
    ├─ T-REX Registry Stack:
    │   ├─ ClaimTopicsRegistry (topics 1,2,3,4,10,99)
    │   ├─ TrustedIssuersRegistry (Datawallet+ registered)
    │   ├─ IdentityRegistryStorage
    │   └─ IdentityRegistry
    ├─ RopeComplianceModule.sol ──► MiFID II rules + AI Testimony
    └─ DCNFTSecurityWrapper.sol ──► DCNFT ↔ ERC-3643 bridge
```

## Contracts

| Contract | Location | Purpose |
|---|---|---|
| `DatawalletClaimIssuer` | `contracts/src/onchainid/` | Issues identity claims from Datawallet+ |
| `IDatawalletClaimIssuer` | `contracts/src/onchainid/interfaces/` | Interface definition |
| `RopeComplianceModule` | `contracts/src/trex/` | MiFID II compliance + AI Testimony |
| `DCNFTSecurityWrapper` | `contracts/src/trex/` | Binds DCNFT to ERC-3643 security token |
| `IDCNFTSecurityWrapper` | `contracts/src/interfaces/` | Interface definition |

## Claim Topics

| Topic ID | Name | Encoded Data |
|---|---|---|
| 1 | `KYC_VALIDATED` | `abi.encode(timestamp, level)` |
| 2 | `AML_VALIDATED` | `abi.encode(timestamp, score)` |
| 3 | `COUNTRY` | `abi.encode(ISO3166_countryCode)` |
| 4 | `ACCREDITED_INVESTOR` | `abi.encode(bool, expiryDate)` |
| 10 | `DCNFT_HOLDER` | `abi.encode(tokenId, contractAddress)` |
| 99 | `SOVEREIGN_IDENTITY` | `abi.encode(did, createdAt)` |

## Deployment Order

```bash
# 1. Deploy ONCHAINID infrastructure
npm run deploy:idfactory -- --network rope_testnet

# 2. Deploy T-REX registries
npm run deploy:trex -- --network rope_testnet

# 3. Deploy compliance stack (ClaimIssuer + ComplianceModule)
npm run deploy:compliance -- --network rope_testnet

# 4. Configure registries (trusted issuer, restricted countries, lockup)
npm run deploy:configure -- --network rope_testnet

# 5. Verify all addresses
npm run verify -- --network rope_testnet
```

## ERC-3643 Transfer Flow

```
1. investor calls token.transfer(recipient, amount)
2. Token checks: paused? frozen?
3. IdentityRegistry.isVerified(recipient.onchainID)
   └─ Checks claims against TrustedIssuersRegistry
   └─ DatawalletClaimIssuer.isClaimValid() returns true/false
4. RopeComplianceModule.canTransfer(from, to, amount)
   └─ Country restriction check
   └─ Max holders per jurisdiction check
   └─ Minimum investment check
   └─ Lockup period check
   └─ Records Testimony on-chain
5. ComplianceAgent (Rust/AI) validates asynchronously
   └─ MiFID II semantic check
   └─ Records external testimony hash
6. Transfer executed, compliance.transferred() updates counters
```

## Testing

```bash
# Unit tests
npm run test:onchainid
npm run test:wrapper

# Integration tests
npm run test:compliance

# Full suite
npm test
```

## Environment Variables

See `.env.example` for the complete list. Key variables:

- `ROPE_RPC_URL` — Datachain Rope RPC endpoint
- `ROPE_CHAIN_ID` — 271828 (mainnet) / 271829 (testnet)
- `ONCHAINID_FACTORY_ADDRESS` — IdFactory CREATE2 address
- `DATAWALLET_CLAIM_ISSUER_ADDRESS` — ClaimIssuer contract

---

*Kazé A. ONGUENE — Datachain Foundation — February 2026*
