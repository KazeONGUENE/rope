# INCIDENT 2026-06-22 — Foundation treasury drain and planned recovery via irregular-state-change hard fork

**Status:** Disclosed 2026-06-30. **Recovery EXECUTED 2026-07-01T18:30Z.** On-chain audit loop **CLOSED 2026-07-02T07:03Z**. All 8,790,904,873.29 FAT moved atomically from the unauthorised recipient's account to the Foundation-controlled rescue wallet `0xCF884C81…082Eb`. On-chain audit trail filed at `UntieRegistry` address `0xEC5975e93eB4fe1Be8d1D60cA997739f521E7a6e`, `record[0]`, with `stateDeltaAppliedAt` now confirmed on-chain (D-3 tx `0xaab4827a…7c2`, block 2,835,515). All 4 production nodes (BLUE + GREEN + DO-1 + DO-2) converged on the recovered state. Rescue wallet balance now: **8,790,912,873.997597 FAT** = pre-funded 8,000.71 residual + 8,790,904,873.29 recovered. See §4-quinquies for the full after-the-fact record. The historical drain transactions at blocks 2,563,361, 2,613,257, and 2,674,893 remain visible in the block explorer — recovery is on top of history, not erasure of it.
**Affected chain:** Datachain Rope mainnet (chainId 271828)
**Affected account:** `0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195` (Datachain Foundation deployer / treasury)
**Funds removed:** 8,790,904,873.29 DC FAT (≈ $5.45M USD at 2026-06-30 reference price of $0.000620/FAT)
**Recipient (unauthorized):** `0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591` — funds still on-chain, unmoved
**Affected ecosystem services:** none at time of writing. DCSwap pools, Tanastok issuance, NaturaProof attestations, Datawallet+ identity, agent attestations all continue to operate normally.

---

## Executive summary

Between 2026-06-22T20:02Z and 2026-06-29T23:51Z, three native DC FAT transactions removed **8,790,904,873.29 FAT** from the Datachain Foundation treasury wallet to a previously-unused external account. The transactions were signed with the deployer private key, which had been documented in plaintext inside the project's workspace configuration files and is therefore considered compromised by an external party.

The Datachain Foundation discovered the discrepancy on 2026-06-30, completed forensic reconstruction the same day, disclosed publicly, and **executed recovery on 2026-07-01**.

The recipient account `0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591` executed zero outbound transactions between receiving the funds on 2026-06-22 and losing them on 2026-07-01. The funds remained entirely on the Datachain Rope chain throughout, and are now visible at zero balance on `https://dcscan.io/address/0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591` — with the full recovery balance visible at the rescue wallet `https://dcscan.io/address/0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb`.

Recovery was effected by a **Tier-S "Sovereign" irregular-state-change** — the same DAO-style mechanism Ethereum used in 2016 — but executed through a permanent, general-purpose primitive: the `UntieRegistry.sol` on-chain audit contract plus the `reth-rope-state-edit` Reth subcommand. Both are now first-class building blocks of the Datachain Rope Phase-1 architecture. Any future recovery — whether Sovereign, Federation, or User-Petition tier — reuses this same code path with tighter authorization thresholds. This incident is exactly the failure mode Phase-1's reversibility property exists to absorb, and the primitive it exercised is now battle-tested.

This document is the authoritative forensic and operational record of the incident, including the on-chain proofs at §4-quinquies. Future updates will be appended here, dated, and signed.

---

## 1. Forensic timeline

All times UTC. Block numbers verified by direct query against the production Reth instance at `https://erpc.datachain.network` on 2026-06-30T06:56Z.

| # | Date / time | Block | Transaction hash | From | To | Native FAT moved |
|---|---|---:|---|---|---|---:|
| 1 | 2026-06-22 20:02:59 | 2,563,361 | `0x7908ca4fa6c99952814f3c63f398bca0fd3266b0d90326900678f4aac732577c` | `0x60FB32ef…4195` | `0xa8bd83cb…60591` | 8,703,006,569.462677 |
| 2 | 2026-06-29 22:32:59 | 2,767,961 | (confirmed by balance delta; tx-detail published in this document's first update) | `0x60FB32ef…4195` | `0xa8bd83cb…60591` | 87,028,023.591818 |
| 3 | 2026-06-29 23:51:38 | 2,769,534 | (confirmed by balance delta; tx-detail published in this document's first update) | `0x60FB32ef…4195` | `0xa8bd83cb…60591` | 870,280.235897 |
| **Total** | | | | | | **8,790,904,873.290392** |

Each transaction was a plain native-FAT transfer (`input = 0x` — i.e. no calldata, no contract interaction) signed with the deployer's secp256k1 private key. Each transaction removed approximately 99% of the sender's balance at that block, leaving 1% behind. This 99% / 1% split is the signature of an automated wallet-drainer script, not of a deliberate operator-driven treasury migration (a human operator would have transferred a round amount, not a percentage of balance).

The sequence of source-account balances around each event was reconstructed by direct `eth_getBalance` calls at each fork-adjacent block:

```
deployer @ 2,563,360:  8,790,915,726.729998 FAT
deployer @ 2,563,361:     87,909,157.267300 FAT   ← drain #1 lands here
deployer @ 2,767,960:                              (~87.91 M; balance unchanged 7 days)
deployer @ 2,767,961:        881,133.6754   FAT   ← drain #2 lands here
deployer @ 2,769,533:                              (~881 k; balance unchanged ~80 min)
deployer @ 2,769,534:          10,853.4395 FAT    ← drain #3 lands here
deployer @ now      :           8,790.71   FAT    (current balance, after gas)
```

And, at the recipient end:

```
suspect @ 2,563,360:                  0.000000 FAT  (account did not exist)
suspect @ 2,563,361:      8,703,006,569.462677 FAT  ← funds arrive
suspect @ 2,767,961:      8,790,034,593.054495 FAT  ← second deposit
suspect @ 2,769,534:      8,790,904,873.290392 FAT  ← third deposit
suspect @ latest   :      8,790,904,873.290392 FAT  ← unchanged since arrival
```

The recipient account has **nonce = 0** at the current head block. No outbound transaction has been signed by the recipient since the funds arrived. The funds are sitting at rest.

## 2. The unauthorized recipient

**Address:** `0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591`
**Type:** Externally-owned account (no contract code)
**Activity:** Three inbound transfers from the Datachain Foundation treasury; zero outbound transactions
**Current balance:** 8,790,904,873.29 DC FAT
**Public label on `dcscan.io`:** none (the address is not registered in any known ecosystem registry — Tanastok, DCSwap, NaturaProof, Datawallet+, Careaway, any of the canonical AI agents, or any ROPE infrastructure)
**Search across Datachain Foundation workspaces:** zero matches. This address does not appear in any source file, environment file, deployment artefact, handover document, or marker file anywhere in the Foundation's project workspaces.

The Foundation explicitly states for the public record: **this address is not under Foundation control and the transfers to it were not authorised by the Foundation, by any of its officers, by any of its contractors, or by any partner project**.

## 3. Root cause

The deployer private key controlling `0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195` was documented in plaintext inside the Foundation's project workspace as part of routine handover documentation between development phases. Specifically the key string appeared in `.cursor/rules/handover-dcswap-redeployed-2026-02-26.mdc` and the cross-project `datachain-rope-production-roadmap.mdc` rule, both of which are "always-applied" Cursor workspace rules that synchronise with cloud-side editor state.

The combined attack surface for plaintext exposure of this key includes, but is not limited to:

1. The operator's local development machine filesystem.
2. Any cloud-side synchronisation of the workspace directory (editor state, settings backup, dotfile sync).
3. Any git remote that received a commit containing the rules files at any point in the workspace's history.
4. Any AI agent transcript in which the rules files were attached as context (the rules are by design always-applied, which means any agent invocation in the workspace received the key string in its input context).
5. The `.env` files of `dcswap-prod` and any other production server that uses the deployer key for routine operations.

The "always-applied" property of the rules — chosen deliberately to keep AI development agents aware of canonical contract addresses, network parameters, and operational invariants — combined with the inclusion of a live private key in the same files, produced an effective long-tail leak of the key across whichever of the above surfaces had been touched at some point between 2026-02-26 and 2026-06-22.

This is a documentation discipline failure, not a protocol failure. The Datachain Rope consensus layer, the Reth execution layer, the cryptographic primitives (BLAKE3, ML-DSA-65, X25519+Kyber768), the V11 RPC method gate deployed 2026-06-12, and the DCSwap timelock deployed 2026-06-12 all behaved exactly as specified. The deployer's signed transactions were valid; the chain correctly executed them. No cryptographic primitive was broken; no smart contract was exploited; no reentrancy or oracle manipulation occurred.

The Phase-2 signed-payload destructive-RPC mechanism that landed in source on 2026-06-13 (`crates/rope-node/src/rpc_signature.rs`) is unrelated to this incident path. That mechanism gates `rope_*` mutator RPCs against unauthorised calls. The drain transactions are native FAT transfers signed off-chain with the deployer key, submitted to `eth_sendRawTransaction` — a path that no public chain can refuse to a holder of the corresponding private key.

## 4. What the attacker did NOT do (yet)

A holder of the compromised deployer key could, until further mitigations land, also do the following:

- **Mint unlimited DCR-20 stablecoins.** The deployer was a `minter` on the `BridgedToken` contracts for USDC (`0xb93b…641f`), USDT (`0x79a2…3289`), and EUROD (`0x24d6…6d4a`). A single transaction can mint an arbitrary amount of any of these to any address. The attacker has not done this.
- **Schedule operations through the DCSwap Timelock.** The deployer holds `PROPOSER_ROLE`, `CANCELLER_ROLE`, and `ADMIN_ROLE` on `DCSwapTimelock` (`0x50Cf…532c`) until the multi-signature Safe is deployed and rotated in. The attacker has not used these powers.
- **Use deployer authority on T-REX / ONCHAINID.** The deployer was the original setup signer for several T-REX infrastructure contracts. The attacker has not interacted with any of these.
- **Move funds through DCSwap pools.** The attacker could swap a portion of the stolen FAT through the FAT/USDC, FAT/USDT, and FAT/EUROD pools, converting it to stablecoin reserves and crashing the FAT price in the process. The attacker has not done this.

The selective use of only the simplest attack vector (a single signed value transfer) is consistent with one of two operator profiles: an automated drain script with a narrow purpose, or a deliberate actor leaving the rest of the surface untouched to remain inconspicuous. Either profile presents continued risk until the key is fully neutralised across the ecosystem.

## 4-bis. Update 2026-06-30T10:00Z — refined 4-hour recovery plan

The recovery has been refined from the original 7-10 day Reth-patch fork to a **3.5-4 hour coordinated MDBX state-edit** that achieves the same irregular-state-change semantics with a smaller surface and a faster window. The rationale: the Foundation controls all 4 production nodes, the funds are still at rest at `0xa8bd83cb…60591`, and the cryptographic primitives needed (`UntieRegistry.sol` for the on-chain audit trail + `rope-state-edit` for the atomic MDBX delta) are smaller than a full Reth fork.

### Refined plan summary

| Component | Status as of 2026-06-30T10:00Z |
|---|---|
| `UntieRegistry.sol` (Solidity contract — public audit trail with Tier-S/F/U authorization model) | Written, lint-clean, at `contracts/src/governance/UntieRegistry.sol`. Tier-S enabled, Tier-F/U coded-but-disabled. |
| `rope-state-edit` (Rust binary — opens Reth MDBX read-write, applies the account delta, recomputes state root, patches block header) | In progress. Depends on `reth-db-api` v1.11.2 (cloned at `/tmp/reth-fork` on rope-vps as fallback reference). |
| Founder-key rotation BEFORE Tier-S use (the existing founder key `eed9f8…a2e3` may share the same exfil vector as the deployer key) | Pending operator: generate fresh Ed25519 on air-gapped device, send back the public half, I patch `master-nodes.toml` + rsync + restart on all 4 master nodes. |
| Rescue wallet `0xCF884C81…082Eb` provenance | Generated on an older laptop **outside the leak surface of this Mac** but software-backed. Used today as a **receive-only address** (zero key access). Hard commitment: **migrate to a hardware-backed `Safe` multi-sig within 72 hours** via one air-gapped outbound signing event from that laptop. |
| nginx log retention | Insufficient — docker stdout-piped logs do not survive `docker logs` rotation. Source-IP of the drain transactions is unrecoverable. Hardening: persist nginx access logs to a separate Docker volume with 90-day retention. |
| Master-node `~/.rope/keys/node.key` file permissions | Found `-rw-r--r--` (world-readable) on rope-vps and dcrope-node2. **Does not affect today's Tier-S recovery** (uses founder key only, not master-node keys; Tier-F kept disabled), but flagged for follow-up: `chmod 600` + rotation under W3-bis. |

### Refined operational sequence

| T+ | Owner | Action |
|---|---|---|
| 0:00 | Operator | Generate fresh founder Ed25519 keypair on air-gapped machine (or YubiKey OpenPGP). Send public key back. |
| 0:10 | Foundation engineering | Patch `master-nodes.toml` with new `FOUNDER_PUB`, rsync to all 4 master nodes (BLUE, GREEN, DO-1, DO-2), restart `datachain-rope.service` on each. Old founder key is now revoked at the protocol level. |
| 0:30 | Foundation engineering | Build `rope-state-edit` Rust binary. Sandbox-test on copy of rope-vps MDBX. Verify state root convergence before/after edit. |
| 1:30 | Operator | Sign deploy tx for `UntieRegistry.sol` using hardware wallet (any pre-funded signer works; the contract has no `owner` other than the configured `consensusOracle`). Receive deployed address. |
| 1:45 | Operator | Sign `UntieRegistry.recordUntie(Sovereign, ..., 0xa8bd83cb…, 0xCF884C81…, 8.79B FAT, prevStateRoot, declaredPostStateRoot, justificationCID, "Foundation treasury drain recovery")` — this is THE permanent on-chain declaration of intent. |
| 2:00 | Foundation engineering | Coordinated stop of `reth-rope.service` on all 4 nodes. ~60-second visible RPC interruption at `erpc.datachain.network`. Failover via nginx upstream is automatic but all 4 backends are down simultaneously. |
| 2:05 | Foundation engineering | Run `rope-state-edit` on each MDBX. Tool produces a verifiable state-root delta proof. The 4 deltas must be byte-identical across all 4 nodes — if any node diverges, recovery aborts and we restart Reth from the original MDBX. |
| 2:20 | Foundation engineering | Restart `reth-rope.service` on all 4 nodes (BLUE first, then GREEN, then DO-1, then DO-2). RPC traffic resumes via nginx upstream. |
| 2:30 | Foundation engineering | Run verification suite: `eth_getBalance(0xa8bd83cb…)` → 0, `eth_getBalance(0xCF884C81…)` → 8.79B FAT, `eth_getLogs(UntieRegistry)` → `UntieRecorded` event present. |
| 2:35 | Operator | Sign `UntieRegistry.confirmStateDelta(0, actualPostStateRoot)` — closes the audit loop by attesting that the actual post-state matches the declared post-state. |
| 3:00 | Foundation engineering | Publish update to this post-mortem with deployed `UntieRegistry` address, `UntieRecorded` tx hash, and full balance-verification proofs. |
| 3:30 | Operator | Tweet / formal announcement: recovery complete. Funds at rest at `0xCF884C81…082Eb` pending Safe migration. |
| Deferred | Operator | Generate `Safe` multi-sig (2-of-3 hardware signers, owners = founder + 2 Foundation board members) **when convenient — the original 72h deadline was cancelled 2026-07-02** (see §4-quinquies "Phase E-final"). When the Safe is generated, sign ONE outbound tx from `0xCF884C81…082Eb` to the Safe via air-gapped signing on the rescue laptop, then rotate the `UntieRegistry.consensusOracle` role to the Safe. |

### Why MDBX-edit instead of full Reth patch

| Property | Reth patch (original plan) | MDBX-edit (refined plan) |
|---|---|---|
| Time to ship | 7-10 days (Reth full build cycle, lockstep deploy, ecosystem-wide patch distribution) | 3.5-4 hours (binary builds in 5-15 min; deploy is `systemctl stop + run + systemctl start`) |
| Audit trail | Built into Reth: state delta visible at block `B_FORK` via Reth's executor logs | Built into `UntieRegistry.sol`: state delta declared in `UntieRecorded` event before MDBX is touched; confirmed in `UntieStateDeltaConfirmed` event after |
| Verifiability by third parties | Patched binary published publicly; anyone can rebuild + verify | `UntieRegistry` events queryable via `eth_getLogs`; before/after state roots both on-chain; anyone can `eth_getBalance` to confirm |
| State-root continuity | Block `B_FORK` has a defined "post-tx-plus-delta" root | Block N has its original root; block N+1 has a root reflecting the modified state. The discontinuity is recorded in `UntieRegistry` |
| Reversibility budget | Same as MDBX-edit — both consume the Phase-1 "reversibility credit" identically | Same |
| Future re-syncing nodes | Need the patched binary | Need either (a) the same MDBX seed snapshot OR (b) a future `rope_untieTx` primitive in `rope-node` that replays the recorded `UntieRecorded` events as state deltas during sync |

The Phase-2 `rope_untieTx` primitive (federation system call + user petition with quorum + Foundation sovereign capability) becomes the **long-term reusable form** of what we exercise today via MDBX-edit. The MDBX-edit is the surgical one-shot; `rope_untieTx` is the regulatory-grade reusable primitive. Both record their actions in the same `UntieRegistry` contract.

### Why the founder key MUST rotate before Tier-S is exercised

The founder Ed25519 (`eed9f8f6fa68d6272fb81229ca311bd0836e38a188d433253adb2d503564a2e3`) is stored at `~/.rope/founder.key` on the founder's Mac per the procedure documented in `master-nodes.toml` lines 122-129. We do not know the exact exfiltration vector that delivered the deployer secp256k1 key to the attacker. The most parsimonious hypothesis is "always-applied workspace rules synced to a place we did not anticipate". If that vector is the only one, the founder key (outside the workspace, at `~/.rope/`) is safe. If the vector is broader (full filesystem read, cloud-side home directory sync, malware), the founder key is also exposed. Prudent default: **assume potentially exposed and rotate before use**. The rotation itself is a single Ed25519 keypair generation on an air-gapped device, a 30-line edit to `master-nodes.toml`, and a 4-node rolling restart. ~15-20 minutes total.

If the new founder pubkey arrives at T+0:05, the entire 4-hour window closes at T+3:30. If the operator needs more time to generate the key, the timer extends accordingly without any other cost.

## 4-ter. Update 2026-06-30T10:30Z — rescue wallet pre-funded, plan simplified

The operator transferred **8,000.709 FAT** from the compromised deployer `0x60FB32ef…4195` to the rescue wallet `0xCF884C81…082Eb` in a single tx, mined around block 2,780,727. On-chain state verified:

```
0xCF884C81…082Eb : 8,000.709454 FAT  (was 0; received this transfer)
0x60FB32ef…4195  :   789.999979 FAT  (was ~8,790; remainder still under attacker risk)
0xa8bd83cb…0591  : 8,790,904,873.290392 FAT  (unchanged from drain; recovery target)
```

**Plan simplification:** the rescue wallet is now both the destination of the recovered FAT AND the `consensusOracle` of `UntieRegistry`. The original plan included a separate single-use "Recovery Operator" EOA funded with ~5 FAT for gas; that step is obsolete because the rescue wallet itself now has 8,000 FAT (about 1,600× the gas budget needed for the 3 transactions: deploy + recordUntie + confirmStateDelta).

**Trade-off accepted:** the rescue laptop's secp256k1 private key signs **4 transactions today** (the inbound transfer, plus 3 recovery operations) instead of 1. The compensating control was originally a hard 72-hour migration deadline to a hardware-backed Safe multi-sig. That hard deadline was **cancelled on 2026-07-02** by founder decision after the recovery landed cleanly through all 4 signings — see §4-quinquies "Phase E-final" for the revised follow-up profile. The Safe multi-sig migration remains the intended end-state but on the founder's timeline rather than a fixed 72-hour clock.

**Compromised deployer remainder (789 FAT):** at attacker-key risk. The operator deliberately did NOT move the full 8,790 FAT because (a) the marginal value vs the recovery target is 0.000009%, (b) any further transaction from `0x60FB32ef…4195` increases the attacker's signal of which counter-wallets are under Foundation control, (c) the recovery target is the 8.79B FAT in the attacker wallet, not the 789 FAT in the deployer remainder. If the attacker drains this 789 FAT, it is an acceptable loss and the recovery proceeds unchanged.

**Updated checklist:** the authoritative operational sequence is now `datachain-rope/docs/RECOVERY_EXECUTION_CHECKLIST_2026-06-30.md`, which supersedes the timeline table in §4-bis above. The 4-hour budget is unchanged; only the funding/signer structure simplified.

## 4-quater. Update 2026-07-01T13:40Z — `postStateRoot` design decision (Option 1: placeholder + confirm)

Choice made explicit for the audit record. `UntieRegistry.recordUntie` accepts a `postStateRootDeclared` parameter (line 291 of `UntieRegistry.sol`). Two viable strategies exist:

- **Option 1 — Placeholder + confirm:** pass `postStateRootDeclared = bytes32(0)` in `recordUntie`; document the placeholder explicitly in `shortReason`; then, after `rope-state-edit` produces the actual post-edit MDBX trie root, call `confirmStateDelta(recordIndex, actualPostStateRoot)` which emits `UntieStateDeltaConfirmed` with `matchesDeclared = false` **by construction**.
- **Option 2 — Pre-compute + confirm:** off-chain simulate the MDBX pass to produce the exact expected post-root, pass it in `recordUntie`, then confirm equality later. Higher fidelity but demanding — the simulation must byte-exactly replicate Reth's `insert_state` encoding, which is fragile to get right.

**Decision (2026-07-01, founder):** Option 1. The `shortReason` field on the Phase D-2 tx carries the phrase `"postStateRoot=0 placeholder; actual via confirmStateDelta"` so any third-party auditor querying `getRecord(0)` sees the same story on-chain. `matchesDeclared = false` on the confirm event is the INTENDED path under Option 1 — it does not indicate tamper. What matters for post-mortem verification is:

1. `prevStateRoot` in `recordUntie` matches the observable head-block `stateRoot` at Phase D-2 landing time (declared honestly, not pre-computed).
2. The `UntieStateDeltaConfirmed` event exists with a non-zero `actualPostStateRoot` (declared honestly, observed post-restart).
3. `eth_getBalance(attacker) == 0` and `eth_getBalance(rescue)` increased by exactly the declared `amount`.

Third parties can verify (1)+(2)+(3) with three `eth_call` / `eth_getLogs` queries — no reliance on the postStateRoot equality check.

**Trade-off accepted:** `matchesDeclared` becomes a de-facto redundant field for one-shot Tier-S recoveries like this one. It is retained in the contract because it is meaningful for FUTURE Tier-F and Tier-U operations where the state-delta path may run entirely in-EVM (e.g. a DCR-20 token rebalance via a signed op-batch) and can pre-compute the exact post-root. The event schema stays uniform across tiers.

---

## 4-quinquies. Update 2026-07-01T18:30Z — recovery **executed**; on-chain and MDBX proofs recorded

Recovery went live on the production fleet on the evening of 2026-07-01. The rest of this document was written before the fact; this section is the after-the-fact record.

### Phase A — founder Ed25519 key rotation (T-3 h)

| Field | Value |
|---|---|
| Old founder pubkey (revoked) | `0xeed9f8f6c1c50d5cd4c9b1fc4dd6bfa4d0ff77e6d9a9d5fefe4b7c9db97ca2e3` |
| New founder pubkey (active) | `0x0e6aa71f8e8161ec7448eca9b04f2e2205b4ef8783810f66cc5c94e4292a77ef` |
| Generated on | operator-owned offline MacBook Pro, no cloud sync, no chat exposure |
| Encrypted at rest | AES-256-CBC PBKDF2 iter=600000, passphrase held on paper only |
| Rotation applied on | rope-vps (BLUE), anvil-vps (GREEN), datachain-rpc-1 (DO-1), datachain-rpc-2 (DO-2) via `patches/founder-key-rotation/rotate-founder-key.sh` |
| Applied at | 2026-07-01T11:22Z on all four nodes; `deploy/config/master-nodes.toml` synced fleet-wide |

### Phase D — on-chain declaration (2026-07-01T17:51Z)

All destructive calls signed **air-gapped** on the same offline MacBook Pro that generated the founder key. The rescue-wallet secp256k1 private key never touched the workspace Mac or any networked machine after Phase D-1's payload was assembled. The `sign_offline.py` script verifies the derived address (`0xCF884C81…082Eb`) matches the expected rescue wallet **before** signing, then processes both unsigned RLP files in a single hidden-input session.

| # | Purpose | Tx hash | Block | Gas used | dcscan |
|---|---|---|---|---|---|
| D-1 | Deploy `UntieRegistry.sol` at deterministic address `0xEC5975e93eB4fe1Be8d1D60cA997739f521E7a6e` (nonce 0 of rescue wallet, CREATE address predicted before signing) | `0x5e204a27dc553fa9ac50a8a976080661d46dcbf218bcc24f4f323d53f7fd7ad9` | 2,819,947 | 1,768,568 | https://dcscan.io/tx/0x5e204a27dc553fa9ac50a8a976080661d46dcbf218bcc24f4f323d53f7fd7ad9 |
| D-2 | `recordUntie(Tier=Sovereign, founderPubkey=0x0e6aa71f…77ef, scope=NativeFat, attacker=0xa8bd83cb…60591, rescue=0xCF884C81…082Eb, amount=8,790,904,873,290,391,388,240,911,228 wei, prevStateRoot=0xa2ee1521…4b24, postStateRootDeclared=0x0 [Option-1 placeholder], shortReason=180 B including "postStateRoot=0 placeholder; actual via confirmStateDelta")` (nonce 1) | `0xb9b6c4eb46c42359aa26021c9455b8cb9067f9adeff623fa1e916c2d7458ef35` | 2,819,949 | 479,000 | https://dcscan.io/tx/0xb9b6c4eb46c42359aa26021c9455b8cb9067f9adeff623fa1e916c2d7458ef35 |

`UntieRecorded` event decoded on-chain:

- topic[0] = `keccak256("UntieRecorded(uint256,uint8,uint8,address,address,uint256,address,uint256,bytes32,bytes32,bytes32,string,bytes32,bytes32)")` = `0x242f3e7ed2a45f0e9d7a83328c5beb549421e5861261f290399fa0a954b0cfec`
- topic[1] recordIndex = `0`
- topic[2] tier = `0` (Sovereign)
- topic[3] attacker = `0xa8bD83cbb72D12209DB2Ac49D4Dc3d78E7760591`
- data amount = `8,790,904,873,290,391,388,240,911,228` wei = full drained balance
- data rescue = `0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb`
- data prevStateRoot = `0xa2ee1521ee69004ea38b3c69561cd4d39d03d836572849e6a3d1f30e09984b24` (block 2,819,949 state root at declaration)
- data postStateRootDeclared = `0x0000000000000000000000000000000000000000000000000000000000000000` (Option-1 placeholder — intentional, disclosed in `shortReason`)
- data founderPubkey = `0x0e6aa71f8e8161ec7448eca9b04f2e2205b4ef8783810f66cc5c94e4292a77ef` (the freshly-rotated key from Phase A)

`recordsLength()` = `1`. `consensusOracle()` = `0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb`.

### Phase E — MDBX state edit + fleet-wide propagation (2026-07-01T18:10Z–18:30Z)

Executed via the Reth `state-edit` subcommand (built in-tree at `patches/reth-state-edit/state_edit_mod.rs`, distributed to all four nodes in Phase B on 2026-06-30/07-01).

BLUE canary:

```
STATE_EDIT_RESULT chain_id=271828
STATE_EDIT_RESULT head_block_before=2820236
STATE_EDIT_RESULT head_state_root_before=0xe450b7fe1f797257453528778e66c274ebcb126050148d5e9d2dea395e2d6c30
STATE_EDIT_RESULT attacker=0xa8bD83cbb72D12209DB2Ac49D4Dc3d78E7760591
STATE_EDIT_RESULT attacker_before_wei=8790904873290391388240911228
STATE_EDIT_RESULT attacker_after_wei=0
STATE_EDIT_RESULT rescue=0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb
STATE_EDIT_RESULT rescue_before_wei=8000707205940869320910
STATE_EDIT_RESULT rescue_after_wei=8790912873997597329110232138
STATE_EDIT_RESULT amount_wei=8790904873290391388240911228
STATE_EDIT_RESULT untie_registry=0xEC5975e93eB4fe1Be8d1D60cA997739f521E7a6e
STATE_EDIT_RESULT untie_registry_record_index=0
```

Arithmetic check: `8,000,707,205,940,869,320,910 + 8,790,904,873,290,391,388,240,911,228 = 8,790,912,873,997,597,329,110,232,138` — exact, no wei lost.

Fleet propagation:

| Node | Method | Post-sync attacker | Post-sync rescue | Notes |
|---|---|---|---|---|
| BLUE | direct `state-edit` on MDBX + restart | 0 | 8,790,912,873.997597 FAT | canary; committed at 2026-07-01T18:10:29Z |
| GREEN | `reth-blue-green-sync.sh` (rsync 4 min) | 0 | 8,790,912,873.997597 FAT | clean on first try |
| DO-1 | forced re-sync (delete mdbx + `rsync --delete`) | 0 | 8,790,912,873.997597 FAT | vanilla `reth-do-sync.sh` skipped mdbx.dat because size matched; forced version worked |
| DO-2 | `reth-do-bootstrap.sh` (tar-stream ~8 min BLUE downtime) | 0 | 8,790,912,873.997597 FAT | MDBX corruption after first sync attempt; bootstrap resolved cleanly |

Fleet-wide verification via public RPC after full convergence:

```
$ curl -sS -X POST https://erpc.datachain.network -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"eth_getBalance","params":["0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591","latest"]}'
{"jsonrpc":"2.0","id":1,"result":"0x0"}

$ curl -sS -X POST https://erpc.datachain.network -H 'content-type: application/json' \
    -d '{"jsonrpc":"2.0","id":1,"method":"eth_getBalance","params":["0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb","latest"]}'
{"jsonrpc":"2.0","id":1,"result":"0x1c67ac35ce52b04142826c4a"}
# 0x1c67ac35ce52b04142826c4a = 8,790,912,873,997,597,329,110,232,138 wei = 8,790,912,873.9976 FAT
```

Rescue-wallet math sanity check:

| Event | Rescue balance delta | Cumulative |
|---|---:|---:|
| Genesis | — | 0 FAT |
| Operator pre-funding (deployer → rescue, before Phase D) | +8,000.709454 FAT | 8,000.709454 FAT |
| D-1 gas (deploy UntieRegistry) | −0.001093 FAT | 8,000.708361 FAT |
| D-2 gas (recordUntie) | −0.001155 FAT | 8,000.707206 FAT |
| Phase E state-edit debit-from-attacker → credit-to-rescue | +8,790,904,873.290391 FAT | **8,790,912,873.997597 FAT** ✓ |

Every FAT is accounted for. The 8,790,904,873.290391 FAT that left the deployer on 2026-06-22 is now back in the Foundation-controlled rescue wallet.

### Phase E-final — `confirmStateDelta` **executed 2026-07-02T07:03Z**; 72-h migration still pending

**D-3 signed on the same offline machine (same paper key, same air-gap USB) and broadcast at 07:03Z on 2026-07-02.** The `UntieStateDeltaConfirmed` event fired on block **2,835,515**, closing the on-chain audit loop opened by D-1 and D-2. Under Option-1 semantics (§4-quater) the `matchesDeclared=false` value on the event is the INTENDED path and does not indicate tamper.

| Field | Value |
|---|---|
| Signer | `0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb` (rescue wallet, verified via `Account.recover_transaction`) |
| Nonce | 2 (D-1 was 0, D-2 was 1) |
| Tx hash | `0xaab4827af4edada00f84d8c988acf4b94113416cf5a9ad2cba907709c2dd07c2` |
| Block | 2,835,515 |
| Gas used | 52,459 (estimate was 53,250 — within 1.5%) |
| Function | `confirmStateDelta(uint256,bytes32)` — selector `0x07afc3db` |
| `recordIndex` | `0` |
| `actualPostStateRoot` | `0xb3d9107ec5577bfa98298ee35e9ba58f6db0243e854e6d1ddf162a03818df864` |
| Event | `UntieStateDeltaConfirmed(recordIndex=0, appliedAtBlock=2835515, actualPostStateRoot=0xb3d9107e…df864, matchesDeclared=false)` |
| Explorer | `https://dcscan.io/tx/0xaab4827af4edada00f84d8c988acf4b94113416cf5a9ad2cba907709c2dd07c2` |

After D-3 confirmed, `records[0].stateDeltaAppliedAt` transitioned from `0` to non-zero on-chain — this is the field a third-party auditor should query via `records(0)` or `getRecord(0)` to verify the recovery lineage has been closed. The `UntieStateDeltaConfirmed` event log at block 2,835,515 is the canonical evidence.

**End-to-end on-chain audit lineage now complete:**

```
D-1 (block 2,819,947)  UntieRegistry deployed        → records[] empty
    │
    ▼
D-2 (block 2,819,949)  recordUntie(Tier-Sovereign)   → records[0] filed
    │                  UntieRecorded event fires        (postStateRoot placeholder = 0x00…00,
    │                                                    stateDeltaAppliedAt still 0)
    ▼
Phase E (~15 min)      reth-rope-state-edit applied  → MDBX delta committed on all 4 nodes
    │                  BLUE, GREEN, DO-1, DO-2 converge  (attacker=0, rescue=8.79B FAT)
    ▼
D-3 (block 2,835,515)  confirmStateDelta(0, root)    → UntieStateDeltaConfirmed event fires
                       records[0].stateDeltaAppliedAt   (actualPostStateRoot=0xb3d9107e…df864,
                       set to block.timestamp             matchesDeclared=false — INTENTIONAL)
```

**72-hour Safe-migration deadline: CANCELLED 2026-07-02T17:45Z by founder decision.** The Safe multi-sig migration remains the intended long-term end-state, but the original hard 72-hour deadline (which would have forced completion by 2026-07-04T18:30Z) is retired. The rationale, from the founder: the recovery is already on-chain and publicly verifiable, the paper-key surface has been operated cleanly through 4 successive signings (initial rescue-wallet inbound transfer + D-1 + D-2 + D-3) with no incidents, and the marginal risk of a further 48 hours on that surface is judged smaller than the operational cost of rushing a Safe generation.

Revised follow-up profile:

- **Safe multi-sig migration → open follow-up, no fixed deadline.** When the founder generates a hardware-backed Safe (2-of-3, owners = founder + 2 Foundation board members), request a fresh unsigned tx blob from the workspace agent for the one-shot outbound transfer of the full rescue-wallet balance (currently ~8,790,912,873.99 FAT) to the Safe address. Same air-gapped signing discipline as D-1 through D-3 (paper key → offline machine → USB → workspace broadcaster). After the transfer confirms, rotate `UntieRegistry.consensusOracle` from the rescue wallet to the Safe via a Foundry-generated `rotateOracle(newOracle=<Safe>)` call. That closes the paper-key chapter permanently.
- **Ongoing paper-key control surface:** until the Safe migration lands, the rescue wallet remains the sole signatory on the 8.79B FAT. This is an explicit, informed acceptance of a single-signature control surface for a paper-key EOA, published here so no future auditor believes the Safe migration was silently skipped.
- **No new operations planned in the meantime.** The recovery is complete; nothing operationally forces another rescue-wallet signature until the Safe migration is executed.

---

## 5. Recovery plan — irregular-state-change hard fork (Option A)

Native DC FAT is the chain's gas token; it has no token contract and therefore no `burn()` method. The native-balance state can only be modified by a holder-signed transaction OR by validator coordination at the consensus layer. Because the funds are at rest, because Datachain Rope's Phase 1 architecture is operated by four Foundation-controlled RPC slots plus two witnesses, and because the production roadmap explicitly preserves "reversibility" as a Phase 1 property pending validator decentralisation, the cleanest available remediation is the **DAO-style irregular-state-change hard fork**.

### 5.1 The mechanism

The Foundation will publish, sign, and deploy a patched build of `reth-rope` (the Foundation's downstream of Reth v1.11.2) that adds one hard-coded rule into the block executor:

> At block `B_FORK`, after the normal execution of all transactions included in that block, apply the following state delta and require the post-state-root of the block to match:
>
> ```
> balance[0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591] -= 8,790,904,873.290392 FAT
> balance[0xRESCUE_WALLET_TO_BE_PUBLISHED_PRIOR_TO_FORK] += 8,790,904,873.290392 FAT
> ```
>
> `B_FORK` is a fixed block height chosen ≥48 hours after the patched binary is published, so the entire ecosystem has time to mirror the patch.

The post-state-root of `B_FORK` is now defined as "the state root produced by tx-execution-plus-this-delta", and every Foundation node running the patched binary will accept exactly that root and reject any alternative. The 4 RPC slots (BLUE, GREEN, rpc-1, rpc-2) plus the 2 witnesses (val-1, val-2) all run the patched binary, so consensus is unanimous by construction.

This is byte-for-byte the pattern Ethereum used in July 2016 at block 1,920,000 to recover funds from the DAO incident. The Ethereum DAO fork is documented, reviewed by ten years of subsequent forensic and academic literature, and remains the canonical reference for irregular-state-change remediation on EVM-compatible chains. Datachain Rope's choice of this pattern is deliberate and is the most conservative recoverable option available.

### 5.2 What is preserved

| Layer | Status |
|---|---|
| L1 — EVM execution state (all account balances, contract bytecode, all storage slots — DCSwap LP positions, ERC-3643 token holdings, ONCHAINID claims, Tanastok DCNFTs, Treasury minter mappings, Timelock pending operations) | Exactly preserved, except the documented two-account delta at `B_FORK`. |
| L2 — Block history (every block, every transaction, every receipt, every log from block 1 to `B_FORK`) | Exactly preserved. Including the three drain transactions, which remain forever visible in chain history. The recovery is recorded **on top of** the chain's history, not as an erasure of it. |
| L3 — Quipu Canon string registry (personal ledgers and per-entity strings) | Same status as today. Already re-bootstrapped on every rope-node restart per the established operational procedure; the fork is one such restart. |
| L4 — Off-chain mirrors (DCSwap indexer Postgres, DCScan caches, Tanastok manifest mirror, IPFS pins, agent indices) | All re-derivable from L1 + L2. Each indexer needs to re-process blocks from `B_FORK` forward, which is a routine operation taking minutes to hours per indexer and runs automatically. |

### 5.3 What changes

The unauthorised recipient's balance becomes zero at `B_FORK`. A newly-generated Foundation rescue wallet (address to be published 48 hours prior to fork) receives the equivalent balance. Block `B_FORK`'s state root is no longer the strict output of tx execution alone — it is the output of tx execution composed with the documented two-account delta. Every later block computes its state root from the post-fork state, so the chain remains internally verifiable forever after `B_FORK`.

### 5.4 What does NOT happen

- No reset of contract addresses. Every contract — WFAT, USDC, USDT, EUROD, the DCSwap Factory/Router/pools, every Tanastok DCNFT and ERC-3643, every ONCHAINID, every agent ledger, the DCSwapTimelock — keeps its current address and current storage.
- No reset of user balances other than the two named accounts.
- No chain restart, no genesis change, no chainId change.
- No loss of transaction history — the three drain transactions remain on chain forever for forensic and regulatory purposes.

### 5.5 Why this is allowed

Datachain Rope's [production roadmap](./QUIPU_CANON_V2_SCALE_5M_TPS_ARCHITECTURE.md), published 2026-05-03, and the umbrella production policy in `.cursor/rules/datachain-rope-production-roadmap.mdc`, explicitly describe the chain's first 12–18 operating months as a Phase-1 "dual-stack with bridge layer", whose stated purpose is to provide:

> Reversibility — If the Rust node has a bug or consensus issue, fall back to Anvil state. … Real traffic validation … State safety. … Reversibility property closes when validator decentralisation completes (Phase 2).

The 2026-03-25 genesis reset, the 2026-03-12 Anvil-to-Reth migration, and the rope-node V11 hot-patch all exercised this reversibility property within its design envelope. The 2026-06-22 incident is the first exercise of it for treasury recovery, and is consistent with the publicly-documented Phase-1 stance.

This option will not be available in Phase 2. The decision to use it now, while it remains available and while the funds remain at rest, is a deliberate single-use exercise of a documented reversibility property in its design window.

## 6. Containment timeline

The Foundation is executing four containment workstreams in parallel with the fork preparation. These workstreams are independent of the fork — they are needed regardless of which recovery option is ultimately chosen, and they close the underlying leak so no second incident is possible on the same vector.

| Workstream | Owner | Status (2026-06-30) | Window |
|---|---|---|---|
| **(W1) Key surface inventory** — locate every system, file, backup, sync target, AI agent transcript, and git remote that ever held the compromised key | Datachain Foundation security | In progress | 0–48 h |
| **(W2) Mitigation of secondary attack vectors** — schedule via the DCSwapTimelock the revocation of the deployer's `minter` role on USDC / USDT / EUROD, and revocation of the deployer's `PROPOSER_ROLE` / `CANCELLER_ROLE` / `ADMIN_ROLE` on DCSwapTimelock (transferring to a hardware-generated rescue key) | Datachain Foundation + DCSwap | In progress | 0–24 h (Timelock min-delay 1 h × 6 operations) |
| **(W3) Founder Ed25519 key audit and rotation** — assess co-exposure of `~/.rope/founder.key` and the contents of `master-nodes.toml`; if co-exposure is confirmed, generate a new founder key on hardware, sign a new `master-nodes.toml`, distribute to all 6 nodes, and anchor a `FounderKeyRotation` knot on the deployer's personal ledger | Datachain Foundation | Scoped, evaluation below | 24–72 h |
| **(W4) DCSwap bot mnemonic rotation** — regenerate the 62-wallet HD seed for the multi-strategy bot, sweep balances to fresh wallets, update the bot's environment, restart bot | DCSwap | Scoped, evaluation below | 48–96 h |
| **(W5) Reth patch + sandbox test of the fork** — patch `reth-rope` executor with the irregular-state-change rule, restore production state snapshot on isolated VM, simulate `B_FORK` execution, verify state root convergence across 4 sandbox nodes | Datachain Foundation engineering | Starts 2026-06-30 | 7–10 days |
| **(W6) Workspace hygiene** — remove every plaintext key from `.cursor/rules/`, rewrite git history of any repo that ever committed a rules file, rotate every key referenced in those files, publish a hardened canonical handover format that bans plaintext key inclusion | Datachain Foundation | In progress | 48–72 h |
| **(W7) Public ecosystem coordination** — notify partner projects (DCSwap, Tanastok, NaturaProof, Datawallet+, Careaway, agents) of fork window; publish reindex instructions; coordinate maintenance window | Datachain Foundation | This document is W7's first step | continuous |

## 7. What this means for ecosystem partners

| Partner | Required action |
|---|---|
| **DCSwap** | (W4) Rotate bot mnemonic; (W2) participate in timelock revocation of deployer minter authority; after `B_FORK`, indexer re-syncs from `B_FORK` automatically. No code changes required. Pool reserves, LP token holders, trading flow all preserved. |
| **Tanastok** | After `B_FORK`, the asset manifest mirror running on dcscan.io will re-warm in ≤5 min. Tanastok Private Pool USDC payout treasury (`0x63423bbc…320B`) is unaffected — it holds DCR-20 USDC, which is not native FAT. The treasury's 5,000 FAT gas float is unaffected. Tanastok issuance, valuation, and DCNFT history are unaffected. |
| **NaturaProof** | Unaffected. Biodiversity verification contracts and pending claims are EVM-state-preserved. After `B_FORK`, any address watchers should expect a one-block reindex window. |
| **Datawallet+** | Unaffected. ONCHAINID identities and Datawallet+ DID-bound personal ledgers are EVM-state-preserved. |
| **Careaway** | Unaffected. Treasury balances and care-plan attestations preserved. |
| **Canonical AI agents** (oracle, insurance, validation, semantic, compliance) | After `B_FORK`, each agent's personal ledger is preserved at the EVM level. The agents' in-memory string state will be re-bootstrapped on the post-fork rope-node restart, which is already an established procedure per the V11 hot-patch deploy. |
| **End users (wallets, exchanges)** | Wallet balances unaffected. RPC continues to serve from `https://erpc.datachain.network`. Brief (≤60 s) RPC pause during fork-block production. |

A separate dated handover with copy-pasteable verification commands will be published in the workspace one week prior to `B_FORK`.

## 8. Lessons and changes to procedure

The Foundation will adopt the following permanent changes to ban this incident's vector class:

1. **No private key, deployer mnemonic, or any other long-lived secret of any kind may ever appear in plaintext in any `.cursor/rules/` file or any always-applied AI agent context.** Where a procedure needs a key, the rule references the operational source-of-truth location (a specific hardware wallet, a specific secrets manager) without quoting the key.
2. **The deployer EOA pattern (single key holding minter, owner, proposer, and treasury authority simultaneously) is replaced by a multi-signature governance model.** The DCSwap Safe deployment, already documented as a pending operator action in the 2026-06-12 timelock handover, is now upgraded to a hard P1 requirement to complete within 30 days of `B_FORK`.
3. **Off-chain monitoring of high-value Foundation accounts** is now mandatory. A watchdog with sub-second alerting on the deployer balance, the bridged-token minter set, and the DCSwapTimelock pending-operation set is being deployed as part of W2.
4. **The Phase-1 reversibility property is now formally classified as a finite, single-use credit.** Every time it is exercised it consumes a portion of the trust budget that backs Phase-2 decentralisation. The Foundation commits to publishing each exercise of the property with full forensic detail (this document being the canonical example).
5. **The Phase-2 signed-payload destructive-RPC mechanism** (already code-complete and gated behind `ROPE_PHASE2_SIGNED_DESTRUCTIVE`) is now scheduled for production deploy within 14 days of `B_FORK`. This does not protect against this exact incident — `eth_sendRawTransaction` of a holder-signed tx is uncontestable — but it tightens every adjacent surface.

## 9. Public verification

Anyone can independently reproduce the full forensic record — from initial drain, through the on-chain declaration, through the recovered state — using only public infrastructure:

```bash
# 1. The three original drain transactions (still visible in the block explorer)
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["0x271d21",true],"id":1}' | \
  jq '.result.transactions[] | select(.from=="0x60fb32ef3a2381c2ed71613f34fd56d56fcf4195")'

# 2. Unauthorised recipient current balance — expected 0 after recovery
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591","latest"],"id":1}'
# Expected: {"result":"0x0"}

# 3. Recipient has still never sent a transaction (Tier-S recovered without their key)
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getTransactionCount","params":["0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591","latest"],"id":1}'
# Expected: {"result":"0x0"}

# 4. Rescue wallet balance — expected 8,790,912,873.9976 FAT (recovered + pre-fund residual)
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getBalance","params":["0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb","latest"],"id":1}'
# Expected: {"result":"0x1c67ac35ce52b04142826c4a"}
# 0x1c67ac35ce52b04142826c4a = 8,790,912,873,997,597,329,110,232,138 wei

# 5. UntieRegistry deployment code + declared record[0]
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getCode","params":["0xEC5975e93eB4fe1Be8d1D60cA997739f521E7a6e","latest"],"id":1}'
# Expected: non-empty bytecode (deployed via D-1 tx 0x5e204a27...7ad9 at block 2,819,947)

# 6. UntieRegistry.recordsLength() — selector 0x82b32d0f
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_call","params":[{"to":"0xEC5975e93eB4fe1Be8d1D60cA997739f521E7a6e","data":"0x82b32d0f"},"latest"],"id":1}'
# Expected: 0x…01 (exactly one record filed)

# 7. UntieRecorded event at block 2,819,949 (Tier-S declaration of the recovery)
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getLogs","params":[{"fromBlock":"0x2b0b7d","toBlock":"0x2b0b7d","address":"0xEC5975e93eB4fe1Be8d1D60cA997739f521E7a6e"}],"id":1}'
# Expected: exactly one log with topic[0] = 0x242f3e7ed2a45f0e9d7a83328c5beb549421e5861261f290399fa0a954b0cfec

# 8. UntieStateDeltaConfirmed event at block 2,835,515 (D-3 — audit loop closed)
curl -sS https://erpc.datachain.network -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","method":"eth_getLogs","params":[{"fromBlock":"0x2b471b","toBlock":"0x2b471b","address":"0xEC5975e93eB4fe1Be8d1D60cA997739f521E7a6e"}],"id":1}'
# Expected: exactly one log with topic[0] = keccak256("UntieStateDeltaConfirmed(uint256,uint256,bytes32,bool)")
# indexed recordIndex = 0 (topic[1] = 0x00…00)
# data = appliedAtBlock=2835515 || actualPostStateRoot=0xb3d9107e…df864 || matchesDeclared=false
```

Steps 1-3 attest the drain and the recipient's continued inability to move the funds; steps 4-7 attest the recovery, the on-chain audit contract, and the Tier-S declaration event; step 8 attests that the audit loop is closed with the observed post-Phase-E state root. Together they constitute an end-to-end publicly-verifiable proof of the incident and its remediation with no reliance on Foundation-provided statements.

This document will be amended (not rewritten) with each operational milestone. Each amendment will be dated and the prior text retained.

## 10. Acknowledgements

The Foundation thanks the Datachain Rope, DCSwap, and Tanastok engineering teams for their disciplined response within the first 24 hours of detection, and the partner projects for their continued operational availability throughout containment.

— Datachain Foundation
2026-06-30
