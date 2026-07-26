# Recovery execution checklist — 2026-06-22 Foundation treasury drain

**Window:** ~3.5–4 hours from T-0 (= the moment FOUNDER_PUB arrives and is rotated in) to T+3:30 (recovery completed, post-mortem updated).
**Owner per row:** OP = human operator action; AGENT = AI agent action; BOTH = synchronous handoff.

## Inputs the operator must provide

| Input | Where used | Status |
|---|---|---|
| `FOUNDER_PUB` (new Ed25519 pubkey, 64 hex chars, no `0x` prefix) | T+0:00 founder rotation | **Pending** — operator chose `later`; live execution Phase D-F gated on this |
| `RESCUE_WALLET_PRIVATE_KEY` (hex, on the air-gapped laptop where it was generated years ago; signs 3 txs total today, then retired within 72h) | T+1:30, T+1:45, T+2:35 | Already exists on the operator's air-gapped laptop. No transfer to rope-vps. |
| `JUSTIFICATION_CID` (IPFS CID of the public post-mortem markdown) | T+1:45 recordUntie | Computed at T+0:55 by pinning the post-mortem to IPFS |

The rescue address `0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb` is committed as both the `consensusOracle` of `UntieRegistry` and the destination of the recovered FAT. Verified on-chain at 2026-06-30 09:42Z: balance is **8,000.709 FAT** (transferred from the compromised deployer in a single tx). Gas budget for the 3 operations (deploy + recordUntie + confirmStateDelta) is ~5 FAT, well within the available balance.

## No separate "Recovery Operator" EOA

The original plan called for a fresh single-use EOA to act as the oracle. The operator's preemptive transfer of 8,000 FAT into the rescue wallet (block 2,780,727 region) makes that unnecessary — the rescue wallet already has the funds, and its key is already "warm" (it just signed the inbound transfer). The simpler design uses the rescue wallet as the oracle directly, then rotates the oracle to a hardware Safe in the 72h migration window.

Trade-off recorded: the rescue laptop key signs 4 total transactions instead of 1. This is acceptable because (a) all 4 signatures happen within a 4-hour window, (b) the laptop should remain offline outside of those signing windows, and (c) the 72h migration to a hardware Safe is non-negotiable per Phase G.

## Operational sequence

### Phase A — founder rotation (~T+0:00 to T+0:30)

| T+ | Owner | Action | Verification |
|---|---|---|---|
| 0:00 | OP | Generate new founder Ed25519 keypair on air-gapped machine (`rope identity init-founder --output ~/.rope/founder-new.key` OR equivalent OpenPGP/YubiKey ed25519). Send PUBLIC key back as 64-char lowercase hex string. Private key NEVER leaves the air-gap. | Pubkey is exactly 64 lowercase hex chars. |
| 0:05 | AGENT | Run `datachain-rope/patches/founder-key-rotation/rotate-founder-key.sh <FOUNDER_PUB>` from the workspace. Patches `master-nodes.toml`, rsyncs to all 6 nodes, rolling restart of `datachain-rope.service`. | Script prints "Founder key rotation COMPLETE." with green status for all 4 master nodes + 2 witnesses. `rope_globalStats` returns `invariant_holds: true`. |
| 0:20 | AGENT | Verify on each node: `ssh <node> 'grep -A4 founder_keys /home/ubuntu/datachain-rope/deploy/config/master-nodes.toml'` shows only the new pubkey. | All 4 nodes show identical `master-nodes.toml`. Old key `eed9f8…a2e3` no longer present. |
| 0:25 | AGENT | Sign a dummy Ed25519 message with the OLD key and submit a `rope_anchorDeployerAttestation` test action. Expect `Authorized::Denied`. | The chain rejects the old key. |
| 0:30 | AGENT | Sign the same message with the NEW key and submit. Expect `Authorized::Founder`. | The chain accepts the new key. Rotation confirmed. |

### Phase B — Reth build (T+0:00 to T+0:45, in parallel with Phase A)

| T+ | Owner | Action | Verification |
|---|---|---|---|
| 0:00 | AGENT | Already running: `nohup bash /tmp/reth-state-edit-patch/apply_and_build.sh > /tmp/reth-state-edit-build.log 2>&1 &` on rope-vps. | `ps aux | grep cargo` shows the build. |
| 0:35-0:45 | AGENT | Wait for build completion (check `/tmp/reth-state-edit-build.log` for "DONE."). | `~/datachain-rope/target/release/reth-rope-state-edit state-edit --help` works. |
| 0:45 | AGENT | Distribute the patched binary to GREEN, DO-1, DO-2 via the `deploy-fleet.sh` pattern (scp + chmod). | Each node has the patched binary at `~/datachain-rope/target/release/reth-rope-state-edit`. |

### Phase C — Logic validation (T+0:45 to T+1:15)

The original plan called for snapshotting the live MDBX into a sandbox and running `--dry-run` against it. That requires either (a) stopping reth-rope on BLUE briefly to get a clean snapshot, or (b) rsyncing a possibly-torn MDBX while reth-rope is running. Option (a) interrupts production for ~30s with no benefit (the snapshot is immediately stale anyway); option (b) risks producing a sandbox MDBX that won't open.

Replaced with **logic validation + canary execution**:

| T+ | Owner | Action | Verification |
|---|---|---|---|
| 0:45 | AGENT | Re-confirm `cargo test -p reth-state-edit-mod` (the unit tests embedded in the patch source) pass on the patched binary. | All tests green. |
| 0:50 | AGENT | Re-run `forge test --match-contract UntieRegistry` to re-confirm all 33 Foundry tests pass on the recovery deploy script + contract. | 33/33 green. |
| 0:55 | AGENT | Verify the patched binary's `state-edit --help` output matches the spec on all 4 nodes (BLUE + GREEN + DO-1 + DO-2 once the GREEN-built binary is distributed). | All 4 nodes print byte-identical help text. |
| 1:00 | AGENT | Document the canary plan for Phase E: BLUE goes first, with full MDBX backup taken IMMEDIATELY before the edit. If BLUE's `STATE_EDIT_RESULT` output looks wrong (wrong attacker_after, wrong rescue_after, or any error), the operator restores BLUE from backup and the recovery aborts before GREEN/DO-1/DO-2 are touched. | Canary plan committed to this checklist; see Phase E updates. |
| 1:10 | AGENT | Verify all 4 nodes are running the SAME pre-edit state root (poll each node's `eth_getBlockByNumber('latest')`); if they're not synchronised, halt and wait for convergence before Phase E. | All 4 report the same state_root at the same block height. |

### Phase D — UntieRegistry deploy + recordUntie (T+1:15 to T+1:50)

The rescue wallet `0xCF884C81…082Eb` is the signer for all three operations in this phase. Operator brings the laptop online ONLY for the signing windows; agent broadcasts the pre-signed hex over the public RPC.

| T+ | Owner | Action | Verification |
|---|---|---|---|
| 1:15 | AGENT | Pin the post-mortem markdown to IPFS (`ipfs add INCIDENT_2026-06-22_FOUNDATION_TREASURY_DRAIN.md`). Capture CID. | CID is a valid base58 string starting with `Qm` or base32 `bafy`. |
| 1:18 | AGENT | Compute the deploy tx parameters: nonce = `eth_getTransactionCount(0xCF884C81…082Eb, 'pending')`, chainId = 271828, gasPrice = `eth_gasPrice()`, value = 0, data = compiled init bytecode + constructor-encoded `(consensusOracle=0xCF884C81…082Eb)`. Estimate gas via `eth_estimateGas` (expected ~3M gas). Hand the operator a single-line "raw to sign" blob. | Blob includes nonce, chainId, gasPrice, gasLimit, value, data. |
| 1:22 | OP | Bring rescue laptop online. Air-gap-sign the deploy raw tx with `0xCF884C81…082Eb`'s private key. Stream the signed hex back. | Hex string starts with `0x02...` (EIP-1559) or legacy `0xf8...`. |
| 1:25 | AGENT | Broadcast the deploy tx via `eth_sendRawTransaction`. Wait for receipt. | Receipt shows `status: 0x1`, `contractAddress: <UNTIE_REGISTRY_ADDR>`. |
| 1:28 | AGENT | Verify on-chain: `cast call <UNTIE_REGISTRY_ADDR> 'consensusOracle()(address)'` returns `0xCF884C81…082Eb`. Also `cast call <UNTIE_REGISTRY_ADDR> 'tierEnabled(uint8)(bool)' 0` returns `true`. | Match. |
| 1:35 | AGENT | Capture the CURRENT head block's state root via `eth_getBlockByNumber("latest")`. This becomes `--declared-prev-state-root` for the state-edit binary and `prevStateRoot` for `recordUntie`. | State root captured (32-byte hash). |
| 1:38 | AGENT | Pre-compute the expected post-state-root by running the state-edit binary in `--dry-run` mode against a **fresh re-clone** of the live MDBX (NOT the sandbox from Phase C — Phase C is a stale snapshot now). Capture the `expected_post_state_root` from its output. | Dry-run prints state root. |
| 1:42 | AGENT | Compute the `recordUntie(...)` raw tx with args: `(tier=Sovereign=0, executiveAuthorityHash=keccak256(FOUNDER_PUB) — or zero if founder-rotation is being deferred and operator accepts the gap, see "Founder gap" note below, federationCommitmentHash=bytes32(0), stateScope=NativeFat=0, assetContract=address(0), debitFrom=0xa8bd83cb…0591, creditTo=0xCF884C81…082Eb, weiAmount=8790904873290392000000000000, prevStateRoot=<captured>, expectedPostStateRoot=<dry-run output>, justificationCid=<bytes32 of IPFS CID>, justificationSummary="Foundation treasury drain recovery — see INCIDENT post-mortem 2026-06-22")`. Hand operator the raw blob. | Blob ready. |
| 1:44 | OP | Air-gap-sign the recordUntie tx. Stream hex back. | Tx is signed by `0xCF884C81…082Eb`. |
| 1:45 | AGENT | Broadcast `recordUntie` tx. Wait for receipt. Confirm `UntieRecorded` event in logs at the deployed UntieRegistry address. | Event emitted, `recordIndex: 0`. |
| 1:50 | AGENT | **POINT OF NO COMFORTABLE RETURN** beyond this step the chain has an on-chain audit record that the recovery is in progress. The state edit MUST follow within the same operational window. | Visible on-chain event at `<UNTIE_REGISTRY_ADDR>`. |

**Founder gap note**: If the operator has not yet provided `FOUNDER_PUB` at T+1:42, the `executiveAuthorityHash` field of the recordUntie call may be set to `bytes32(0)` and the recovery proceeds without on-chain commitment of the founder authority. The audit trail is weaker (no on-chain Ed25519 pubkey commitment). When the founder key is rotated later, an addendum can be published in the post-mortem committing the new pubkey, but the on-chain UntieRegistry record cannot be retroactively updated. Recommended: pause Phase D until FOUNDER_PUB arrives. The Reth build + sandbox dry-run (Phases B-C) can complete in the meantime.

### Phase E — State edit + chain restart (T+1:50 to T+2:30)

Two-stage canary pattern: BLUE first, observe, then propagate to the others. The state-edit operation is deterministic, so byte-identical input on identical Reth code paths against identical MDBX state MUST produce byte-identical output. Any deviation between BLUE and the other 3 is a sign that something is wrong with the inputs (e.g. state-root drift between nodes), and the recovery must halt.

| T+ | Owner | Action | Verification |
|---|---|---|---|
| 1:50 | AGENT | On all 4 nodes: `systemctl stop reth-rope.service`, `tar -czf /opt/datachain-rope/backup/mdbx-pre-edit-$(date +%s).tgz /opt/datachain-rope/reth/data/db/mdbx.dat /opt/datachain-rope/reth/data/static_files/`. RPC traffic stops; nginx upstream returns 502 until restart (this is the visible window). | 4 backup files exist, sizes proportional to the MDBX. |
| 1:55 | AGENT | **Stage 1 — BLUE canary:** Run `reth-rope-state-edit state-edit --datadir /opt/datachain-rope/reth/data --attacker 0xa8bd83cbb72d12209db2ac49d4dc3d78e7760591 --rescue 0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb --amount-wei 8790904873290392000000000000 --declared-prev-state-root <root_from_Phase_D> --untie-registry-address <addr> --untie-registry-record-index 0 --justification-cid <CID> --i-have-read-the-untie-registry-event "I have read the UntieRegistry event on chain 271828"` on BLUE only. | BLUE prints `STATE_EDIT_RESULT` lines with `attacker_after_wei=0`, `rescue_after_wei=8790904873290392000000000000`. |
| 2:00 | AGENT | **CANARY VERIFICATION:** read BLUE's STATE_EDIT_RESULT output. The amount, attacker, rescue, and state root must match the Phase-D recordUntie event's declared values exactly. If ANY field differs, restore BLUE from backup, restart, and HALT. | All fields match. |
| 2:05 | AGENT | **Stage 2 — propagate:** Run the SAME command (byte-identical args) on GREEN, DO-1, DO-2 in parallel. | Each node prints its own `STATE_EDIT_RESULT`. |
| 2:15 | AGENT | **DIVERGENCE CHECK:** compare BLUE's STATE_EDIT_RESULT to GREEN's, DO-1's, DO-2's. They must be byte-identical (same attacker_before, same rescue_before, same head_state_root_before, same head_block_before). | All 4 identical. Any divergence → restore ALL 4 nodes from backup and HALT. |
| 2:18 | AGENT | Restart `reth-rope.service` on all 4 nodes (BLUE first, then GREEN, then DO-1, then DO-2). | All 4 nodes resume block production. |
| 2:25 | AGENT | Wait for first new block after restart. Verify on each node individually via direct RPC: `eth_getBalance(0xa8bd83cb…) == 0`, `eth_getBalance(0xCF884C81…) == 8790904873290392000000000000 + 8000709453508885053886` (rescue balance is sum of the original 8,000 FAT pre-fund plus the recovered 8.79B FAT; precise value depends on gas spent in Phase D, see footnote). | Balances match expected across all 4 nodes. |

Footnote on rescue balance arithmetic: at Phase E start the rescue balance is 8,000.709 FAT minus whatever gas was spent in Phase D (deploy + recordUntie ≈ 4 FAT). After Phase E, the rescue balance is that residual + the recovered 8,790,904,873.290 FAT. The exact integer to verify against is `eth_getBalance(0xCF884C81…)` at the moment of the first post-restart block; record it in the post-mortem during Phase F.

### Phase F — Audit-trail closure + announcement (T+2:30 to T+3:30)

| T+ | Owner | Action | Verification |
|---|---|---|---|
| 2:30 | AGENT | Capture the new head block's state root. | Root captured. |
| 2:30 | AGENT | Compute the `confirmStateDelta(0, actualPostStateRoot)` raw tx with `0xCF884C81…082Eb`'s next nonce. Hand operator the raw blob. | Blob ready. |
| 2:32 | OP | Bring rescue laptop online one last time today. Air-gap-sign `confirmStateDelta(0, actualPostStateRoot)` tx. Stream hex back. **After this signing window, take the laptop offline until the 72h Safe-migration step.** | Signed. |
| 2:35 | AGENT | Broadcast `confirmStateDelta`. Wait for receipt. Confirm `UntieStateDeltaConfirmed` event with `matchesDeclared: true`. | Event emitted, `matchesDeclared: true`. |
| 2:40 | AGENT | Run the full public verification suite from `docs/INCIDENT_2026-06-22_FOUNDATION_TREASURY_DRAIN.md §9`. | All checks pass. |
| 2:50 | AGENT | Update post-mortem: add deployed `UntieRegistry` address, deploy tx hash, `recordUntie` tx hash, `confirmStateDelta` tx hash, before/after state roots, balance snapshots. | Markdown committed. |
| 3:00 | AGENT | Re-pin the updated post-mortem to IPFS. Capture new CID. Anchor a `RecoveryComplete` knot on the rescue wallet's personal ledger via `rope_appendToLedger` containing the new CID and all tx hashes. | Knot anchored, visible at `https://semantic-agent.datachain.network/v1/search?q=RecoveryComplete`. |
| 3:15 | OP | Public announcement (tweet, mailing list, partner notifications per the post-mortem). | Published. |
| 3:30 | BOTH | Recovery operation complete. | All artifacts published. |

### Phase G — 72h hardware migration (after T+3:30, up to T+72h)

| Window | Owner | Action |
|---|---|---|
| T+24h to T+72h | OP | Generate a Safe multi-sig with hardware-backed signers (Ledger / YubiKey / etc.). |
| T+24h to T+72h | OP | On the rescue laptop, air-gapped, sign ONE outbound tx from `0xCF884C81…082Eb` transferring all funds to the Safe. Permanently destroy the laptop key after. |
| T+72h | OP | Rotate `UntieRegistry.consensusOracle` from `RECOVERY_OPERATOR_ADDR` to the Safe via `rotateOracle(safeAddr)`. Permanently destroy `RECOVERY_OPERATOR_PRIVATE_KEY` after. |
| T+72h | AGENT | Update post-mortem with Safe address + migration tx hash + oracle rotation tx hash. |

## Failure modes and rollback

| If at step | Then |
|---|---|
| Phase A founder rotation fails (any node refuses to restart) | Halt; investigate. The compromised key is not rotated; the recovery is delayed. The drain is at rest; there is no time pressure. |
| Phase B Reth build fails | Halt; debug compile errors. Optionally fall back to a Reth-source-patch approach (the patch source is in `datachain-rope/patches/reth-state-edit/state_edit_mod.rs`; build errors are localised to that file). |
| Phase C sandbox dry-run produces unexpected state | Halt; do NOT touch production MDBX. Iterate on the state-edit code until sandbox produces the expected output. |
| Phase D contract deploy reverts or gas runs out | Halt; refund recovery operator if needed. Re-deploy with a fresh nonce. |
| Phase D recordUntie reverts | Halt; debug. The on-chain audit trail is NOT recorded; state edit must not proceed. |
| Phase E `STATE_EDIT_RESULT` divergence across 4 nodes | Restore ALL 4 from `mdbx-pre-edit-*.tgz` backups. Restart Reth. Recovery is rolled back. UntieRegistry has a recorded-but-not-confirmed entry that will be marked as such forever (this is its design). |
| Phase F balance verification fails on any node | Halt; restore from backup; do not call `confirmStateDelta`. |

The single hard rule throughout: **the 4 nodes must converge or all 4 revert. No partial state on the chain.**

## Status of inputs (live, this document is the source of truth)

| Input | Provided? |
|---|---|
| FOUNDER_PUB | ⏳ operator chose `later` — Phase A and Phase D recordUntie's `executiveAuthorityHash` field gated on this |
| Rescue wallet 0xCF884C81…082Eb funded with FAT | ✅ done — 8,000.709 FAT on-chain at block 2,780,727 area; rescue wallet IS the consensusOracle (no separate Recovery Operator EOA needed) |
| Reth source cloned on rope-vps at `/tmp/reth-fork` | ✅ done |
| state-edit patch source written | ✅ done (`datachain-rope/patches/reth-state-edit/`) |
| state-edit Rust binary built | 🔄 building (background, see `/tmp/reth-state-edit-build.log`) |
| UntieRegistry.sol written + tests passing | ✅ done (33/33 forge tests green) |
| DeployUntieRegistry.s.sol written | ✅ done (`contracts/scripts/DeployUntieRegistry.s.sol`) |
| Founder-rotation script written | ✅ done (`datachain-rope/patches/founder-key-rotation/rotate-founder-key.sh`) |
| Post-mortem written + refined plan committed | ✅ done (`datachain-rope/docs/INCIDENT_2026-06-22_FOUNDATION_TREASURY_DRAIN.md`) |
| Sandbox dry-run procedure documented (this file, Phase C) | ✅ done |

— Datachain Foundation, 2026-06-30
