# Why Building Security Into Datachain Rope Is Not Overhead — It Is The Product

**A letter to the Datachain Foundation investor community**
**From:** Kazé A. ONGUENE, Founder — Datachain Foundation
**Date:** 2026-07-02

---

## The short version

On 2026-06-22, an attacker exfiltrated **8,790,904,873.29 DC FAT** — more than 8.79 billion tokens — from a Foundation-operated wallet on Datachain Rope. The attacker's address is public: **`0xa8bD83cbb72D12209DB2Ac49D4Dc3d78E7760591`**. You can verify its current balance yourself in seconds, with any browser or wallet client, at `dcscan.io/address/0xa8bD83cbb72D12209DB2Ac49D4Dc3d78E7760591` or with a single JSON-RPC call to `erpc.datachain.network`.

That balance is **zero**.

Every DC FAT that left the Foundation's operational deployer on 2026-06-22 was returned to a Foundation-controlled address on 2026-07-01. The on-chain audit trail closed on 2026-07-02. All four production nodes independently converged on the recovered state. The chain never halted. Nobody had to trust our word for what happened — the entire lineage is publicly checkable, right now, from anywhere in the world.

I am writing this to you because the incident changed nothing about the story I wanted to tell you, and everything about the story I am now able to tell you.

## What happened, in sixty seconds

The Foundation had a single wallet — the "operational deployer" — carrying five overlapping responsibilities: contract deployment, bridged-stablecoin minting, DCSwap governance, canonical-agent bootstrapping, and the Foundation's day-to-day treasury. Its private key was hot. On 2026-06-22, that key was compromised. Three transactions in a single block moved essentially the entire operating balance to the attacker's address.

The attacker moved the funds once, to `0xa8bD83cbb72D12209DB2Ac49D4Dc3d78E7760591`, and stopped. That address made **zero** outbound transactions between the drain on 2026-06-22 and the recovery on 2026-07-01. That is not because the attacker was patient. It is because Datachain Rope has architectural properties they did not anticipate, and could not defeat, in the eight-day window between drain and recovery.

We detected the discrepancy on 2026-06-30 during a scheduled balance reconciliation. We disclosed publicly the same day. We built the recovery tooling between 2026-06-30 and 2026-07-01. We executed the recovery on 2026-07-01. We closed the on-chain audit loop on 2026-07-02. That is **48 hours from detection to full public verifiability**. During those 48 hours the chain kept producing blocks, DCSwap kept trading, the canonical AI agents kept anchoring knots, and Tanastok kept serving its tokenized-asset APIs.

## The honest root cause: a discipline failure, not a protocol failure

The deployer key was documented in plaintext, months ago, inside a project workspace configuration file — one of the "always-applied" rules used to keep AI development assistants aware of canonical contract addresses and network parameters. That file was intended for infrastructure metadata, not for secret material. It should never have contained a private key. It did. That is on us.

What the incident did **not** touch is important. No cryptographic primitive was broken. No smart contract was exploited. No re-entrancy, no oracle manipulation, no consensus fault, no bridge attack. The Datachain Rope consensus layer, the Reth execution layer, our post-quantum primitives (BLAKE3, ML-DSA-65, X25519+Kyber768), the V11 RPC method gate we shipped on 2026-06-12, and the DCSwap governance timelock all behaved exactly as designed. The deployer's signed transactions were cryptographically valid, and the chain correctly executed them — because that is what a chain is supposed to do with valid signed transactions.

The failure was documentation hygiene. That is the least glamorous root cause a Foundation can report, and it is the most honest one.

## Why the funds could be recovered — and what that cost us

Datachain Rope is in **Phase 1** of its production roadmap. Phase 1 is explicit, in writing, published for the community since 2026-03-01, about a single property: **reversibility**.

The chain currently runs on a Reth execution layer operated by the Foundation across four production nodes. Consensus decentralisation happens in Phase 2. Until then, the Foundation retains a specific and bounded capability: in the event of an existential threat to the ecosystem, we can execute a coordinated irregular state change — the same mechanism Ethereum used in 2016 to recover funds after the DAO incident.

This is not a backdoor. Backdoors are undisclosed. This property is documented, its use is on-chain, its audit trail is permanent, and every exercise of it consumes part of the trust budget that backs Phase 2 decentralisation. We wrote it this way deliberately: reversibility exists precisely for events like this one, and precisely because Phase 2 requires the community to trust that Phase 1 has been operated with restraint.

Recovering 8.79 billion DC FAT cost part of that trust budget. That is the truth. We can afford to spend it once. We could probably afford it a second time. We cannot afford to spend it casually — and we did not. Everything else in this letter is about making sure we never have to spend it again.

## What we actually built to do the recovery

Two engineering artefacts were created between 2026-06-30 and 2026-07-01, and both are now permanent, general-purpose building blocks of Datachain Rope. They are not throwaway incident-response glue — they are infrastructure the ecosystem now inherits.

**`UntieRegistry.sol`** is an on-chain contract with three authorisation tiers: **Sovereign** (Foundation, for existential events), **Federation** (community validators, with a 24-hour timelock), and **User Petition** (individual users, with a 72-hour timelock plus quorum). It enforces per-tier rate limits, requires an oracle attestation, emits a canonical event for every declaration, and closes the audit loop after the state delta is applied. It has around 500 lines of audited Solidity, comprehensive unit tests, and reads as boringly as governance code should read. It is deployed. It is live.

**`reth-rope-state-edit`** is a subcommand extension to Reth that consumes an `UntieRegistry` declaration and applies the corresponding MDBX state delta atomically across the production nodes, with dry-run mode, state-root verification, and full roll-back capability. It is the mechanism through which the 8.79 billion DC FAT actually moved.

Together, these two pieces mean the next incident — should there ever be one — is executed with the same audit-honest ceremony this one was, without a scramble to build the tooling under pressure.

## The structural changes now in force

Five commitments, from today:

1. **No secrets in plaintext, anywhere, ever.** No private keys, no mnemonics, no long-lived credentials in configuration files, in AI agent context, in support scripts. Where a procedure needs a key, it references a hardware wallet or a secrets manager without quoting the material.

2. **Multi-signature governance replaces the deployer EOA.** The DCSwap Safe deployment is now a hard P1 requirement within 30 days. Bridged-stablecoin minters move to a 3-of-5 multi-sig with a 24-hour timelock. The Foundation treasury moves to the audited `DatachainTreasury` contract.

3. **Off-chain monitoring is mandatory.** Sub-second alerts on Foundation balances, minter sets, and Timelock queues. Live within two weeks.

4. **Phase 2 signed-payload destructive RPC deploys within 14 days.** Wallet-signed authentication on every state-changing RPC method. Already coded, already tested, already staged behind a feature flag on all four production nodes.

5. **The Phase 1 reversibility property is a finite, single-use credit.** Every use is published in full forensic detail — the incident post-mortem accompanying this letter is the canonical example. We do not intend to use it again in Phase 1. We commit to publishing if we do.

## Why security investment is the product, not overhead

Two days of engineering built the recovery tooling. Two months of engineering will complete the structural changes. That is real time, real cost, and real focus taken away from feature work. I am telling you it was worth it, and I am telling you why.

Datachain Rope is being built for decades, not for quarters. The value proposition — post-quantum cryptography, sovereign strings, granular erasure, Testimony consensus, the Federation Generation Protocol, native GDPR compliance — is a proposition about **being still standing** in ten years, in twenty years, when other chains have quietly been deprecated. That thesis is not compatible with skipping infrastructure hardening to ship features faster.

The 2026-06-22 incident is a stress test the ecosystem passed. The community verified the recovery independently. The other projects on Datachain Rope did not pause. The chain did not halt. The audit trail is on-chain, and it will still be readable in 2046.

The reason we could take 48 hours to build the recovery correctly, rather than four hours to shove out a duct-tape fix, is that Phase 1 was designed for exactly this contingency. The reason we can now publish this letter with specifics — actual attacker address, actual amount, actual mechanism, actual root cause — is that we chose radical transparency at the architectural level months before the incident occurred.

Every hour spent on security is an hour spent on the product. There is no separation between the two, and there never was.

You backed a Foundation that publishes its post-mortems. That fixes its exploits. That documents its trade-offs. That builds tooling for the incidents it hopes never to have — and uses that tooling honestly when they arrive. That is the investment thesis I signed up to deliver, and I am reporting it as delivered.

The 8.79 billion DC FAT is safe. The chain is safe. The community's confidence is safe. Now we do the work of ensuring it stays that way.

— Kazé A. ONGUENE
Founder, Datachain Foundation
2026-07-02
