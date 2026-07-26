# Security Review — Recurrence Risk of the 2026-06-22 Treasury Drain

**Author:** Datachain Rope agent (workspace review)
**Date:** 2026-07-02
**Scope:** Find breaches that could cause an incident of the same class as the 2026-06-22 Foundation treasury drain (plaintext-key exposure → unauthorised signed transactions / privileged calls).
**Method:** Static inspection of the workspace (`.cursor/rules/`, `deploy-scripts/`, `contracts_vps.env`, `crates/rope-node/`, mirrored trees `datachain-rope-p2b/`, `datachain-rope-deploy/`, `dc-explorer-build-2026-03-07/`). Live-chain confirmations were interrupted by a terminal outage and are flagged **[VERIFY ON-CHAIN]** with the exact command to run.

> **Bottom line:** the 2026-06-22 remediation is **incomplete**. The private key the post-mortem itself declares "compromised" is **still present in plaintext in this workspace in at least 16 files**, including an always-applied Cursor rule, and (per the DCSwap timelock handover) that key is **still a live minter** on the USDC/USDT/EUROD bridged stablecoins. This is not a hypothetical recurrence — it is the *same* exposed secret with a *second*, still-open, higher-leverage attack path (unlimited stablecoin mint). Treat CRITICAL-1 and CRITICAL-2 as an active incident continuation, not a future risk.

---

## Severity summary

| # | Severity | Finding | Same class as 2026-06-22? |
|---|---|---|---|
| C-1 | **CRITICAL** | The compromised deployer key `0x659f…63a88` is still in plaintext across ≥16 workspace files, including an always-applied rule | Identical root cause |
| C-2 | **CRITICAL** | That same key is (per handover) still an active `minter` on USDC/USDT/EUROD → unlimited stablecoin mint | Same key, worse blast radius |
| C-3 | **CRITICAL** | `deploy-scripts/*.js` hardcode the compromised key as a default and point at production RPC `erpc.datachain.network` | Same root cause, executable form |
| H-1 | HIGH | `contracts_vps.env` contains the compromised key in plaintext on disk | Same root cause |
| H-2 | HIGH | Other live secrets referenced by handovers (`TANASTOK_ISSUER_PRIVATE_KEY`, `PRIVATE_POOL_PAYOUT_PRIVATE_KEY`) follow the same "key in env, described in rules" pattern | Same root cause, adjacent systems |
| H-3 | HIGH | Phase-2 signed-RPC gate is coded but **OFF**; the only thing protecting destructive `rope_*` methods is the Phase-1 IP/loopback heuristic | New surface |
| M-1 | MED | `reth-rope-state-edit` is a balance-rewrite primitive whose safety rests entirely on one paper-key oracle wallet | New surface created by the recovery |
| M-2 | MED | Rescue wallet is still a single paper-key EOA holding 8.79B FAT (Safe migration deadline was cancelled) | Direct successor risk |
| M-3 | MED | Three mirrored source trees (`-p2b`, `-deploy`, `dc-explorer-build-*`) duplicate every leaked secret and drift independently | Amplifies C-1 |
| L-1 | LOW | No secret-scanning pre-commit / CI guard exists to prevent the next key from being pasted into a rule | Prevention gap |

---

## C-1 — The compromised key is STILL in the workspace in plaintext

The post-mortem (§3) names the leak vector as the key string inside `handover-dcswap-redeployed-2026-02-26.mdc` and `datachain-rope-production-roadmap.mdc`. That key is:

```
<REDACTED-COMPROMISED-KEY-see-SECURITY_AUDIT_2026-07-25>
```

It is still present, unredacted, in at least these files (grep-confirmed 2026-07-02):

- `.cursor/rules/handover-dcswap-redeployed-2026-02-26.mdc:323`  ← **always-applied rule** (loaded into every agent context in this workspace)
- `.cursor/rules/handover-dcswap-project-status.mdc:327`
- `.cursor/rules/reth-migration-2026-03-12.mdc:135`
- `contracts_vps.env:9`
- `deploy-scripts/` — 12 files (see C-3)
- `datachain-rope-p2b/.cursor/rules/handover-dcswap-redeployed-2026-02-26.mdc:177`
- `datachain-rope-deploy/.cursor/rules/handover-dcswap-redeployed-2026-02-26.mdc:323`

Because `handover-dcswap-redeployed-2026-02-26.mdc` is `alwaysApply: true`, this key is **re-exposed to every AI agent invocation and every editor-state sync**, exactly the mechanism the post-mortem blamed for the original leak. Rotating the *founder* key (done) and recovering the *drained balance* (done) did **not** neutralise this: the key still signs for whatever authority it still holds (see C-2). The recovery moved the FAT out of reach, but the key was never de-authorised everywhere and was never removed from the workspace.

**[VERIFY ON-CHAIN]** confirm the address + residual authority:
```bash
# derive address (offline): 0x659f… → expect the deployer 0x60FB32…
# then check it holds no lingering roles:
cast wallet address --private-key <REDACTED-COMPROMISED-KEY-see-SECURITY_AUDIT_2026-07-25>
```

**Why still CRITICAL after recovery:** the key's *balance* is gone but its *capabilities* are not. See C-2.

---

## C-2 — The compromised key is still a live stablecoin minter

`handover-from-dcswap-timelock-live-on-271828-2026-06-12.mdc` explicitly states the deployer's `minter` flag on the bridged tokens was **"preserved on purpose"** so the trading bot's mint pipeline keeps working:

> `*.minters[deployer]` `true` → **`true`** (preserved on purpose)
> "The deployer's minter role is the one piece of authority that's still single-key. It will be revoked … once we either (a) … or (b) … That's a follow-up after the Safe is deployed; not urgent."

Combined with C-1, this is the live danger. The post-mortem itself (§ "secondary attack vectors") says a holder of this key can **mint unlimited USDC/USDT/EUROD to any address**. The attacker demonstrably still had this key on 2026-06-22. It has not been rotated out of the minter set. So the exact same attacker can, at any moment:

1. `BridgedToken.mint(attacker, 10^30)` on USDC/USDT/EUROD, then
2. dump the minted stables into the DCSwap FAT/USDC, FAT/USDT, FAT/EUROD pools, draining real FAT liquidity and destroying the peg.

This is a **larger** blast radius than the original native-FAT drain, and it is reachable **today** with a key that is sitting in an always-applied rule. The "not urgent" classification in the June-12 handover predates the June-22 drain and is no longer valid — the key is now known-compromised.

**[VERIFY ON-CHAIN]** (highest priority):
```bash
RPC=https://erpc.datachain.network
DEPLOYER=0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195   # confirm this is 0x659f's address first
for T in 0xb93bd8db94f1baff474aa9cba0739daaad01641f \
         0x79a26132f48394421382c13b54ae77fa3af73289 \
         0x24d6137807fa8a592888726d87ac748d018c6d4a; do
  echo -n "$T minter? "; cast call $T "minters(address)(bool)" $DEPLOYER --rpc-url $RPC
done
```
If any returns `true`, the ecosystem has an open, unlimited-mint hole gated only by a leaked key.

---

## C-3 — Deploy scripts hardcode the compromised key + target production

Every script in `deploy-scripts/` embeds the compromised key and defaults to production RPC:

- `deploy_dcnft_only.js:11`, `deploy_trex_factory_and_dcnft.js:21`, `fix_tanastok_claims.js:11`, `fix_tanastok_identity.js:15`, `deploy_trex_infra_and_register_tanastok.js:18`, `redeploy_trex_fin1_full.js:20` — `const DEPLOYER_KEY = "0x659f…63a88";` with `RPC_URL … || "https://erpc.datachain.network"`.
- `publish_token_to_dcswap.js:17`, `task1/2/4_*.js` — same key hardcoded.
- `deploy_dcli.js:25-26` — uses it as a *fallback default* if `DEPLOYER_KEY` env is unset, and points at `erpc.datachain.network`. A well-meaning `node deploy_dcli.js` with no env set will sign a **production** deploy with the **compromised** key.

This is C-1 in directly-executable form. Even after the key is rotated, these scripts will keep trying to use the dead key against prod, and they normalise the anti-pattern of committing keys.

---

## H-1 — `contracts_vps.env` holds the key on disk

`contracts_vps.env:9`:
```
DEPLOYER_PRIVATE_KEY= <REDACTED-COMPROMISED-KEY-see-SECURITY_AUDIT_2026-07-25>
```
An `.env` named like a deployable artefact, containing the compromised key, with no evidence it is git-ignored (**[VERIFY]** `git check-ignore contracts_vps.env` — if it prints nothing, it is tracked and may be in remote history). Note the post-mortem's own attack-surface list item 3: "Any git remote that received a commit containing the rules files."

---

## H-2 — Same "key-in-env, described-in-rules" pattern on adjacent systems

The pattern that produced the incident is repeated for other live keys, and those descriptions live in always-applied rules:

- `handover-tanastok-quipu-canon-v1.2-live-2026-05-21.mdc:147` documents `TANASTOK_ISSUER_PRIVATE_KEY="0xe4b2…"` (partial, but the pattern + location are the risk). The Tanastok issuer holds `MINTER_ROLE` on DCNFT/ERC-3643 per the tokenized-assets handover.
- `handover-from-tanastok-treasury-and-rope-token-reconciliation-2026-06-04.mdc` and the treasury-confirmed handover describe `PRIVATE_POOL_PAYOUT_PRIVATE_KEY` for a wallet that (per the funding handover) received **5,000,000 USDC**.

None of these show the *full* key in the workspace today (good), but they institutionalise "secrets belong in prose handovers." The rule `.cursor/rules/handover-*.mdc` files are the exact surface the post-mortem flagged. A single future paste of a full key into one of these reruns 2026-06-22.

**[VERIFY]** that the referenced `.env.production` files on `tanastok-vps` are `chmod 0600` and were never echoed into a rule at full length.

---

## H-3 — Destructive-RPC protection is a heuristic, not a signature (Phase-2 OFF)

`crates/rope-node/src/rpc_auth.rs` is well built and fail-secure, but its **only** active control is the Phase-1 gate:

- `should_deny()` denies the 5 destructive methods **unless** `public_destructive_deny_enabled()` is false, and the bypass is `peer.is_loopback() && !has_x_forwarded_for` OR a matching `X-Rope-Internal-Token`.
- `phase2_signed_destructive_enabled()` (real per-call secp256k1/Ed25519 signature verification) defaults **OFF** (`ROPE_PHASE2_SIGNED_DESTRUCTIVE` unset).

Residual risks while Phase-2 is OFF:
1. The loopback-without-XFF bypass trusts that **nginx always sets `X-Forwarded-For`**. If any future reverse-proxy/config change, a misconfigured additional listener, or an SSRF-from-a-co-located-service ever reaches `127.0.0.1:8545` without an XFF header, it inherits full destructive authority (create/erase ledgers, untie knots, anchor deployer attestations).
2. `deactivateTier`/`rotateOracle`-class power is not in this gate at all — it lives in `UntieRegistry` (see M-1).
3. `DESTRUCTIVE_METHODS` is a hand-maintained allowlist; the guarding test `rpc_auth_destructive_list_locked` only fires if someone updates the expected-list — a new mutator added without touching either list is unguarded. **[VERIFY]** the dispatcher has no mutator missing from this array.

This is not the 2026-06-22 vector, but it is the nearest adjacent one: it is the surface that would let a *network* attacker (vs a key holder) reach state mutation.

---

## M-1 — `reth-rope-state-edit` is a god-mode primitive gated by one paper key

The recovery created two permanent, extremely powerful tools:

- `UntieRegistry.sol` — `recordUntie` / `confirmStateDelta` / `rotateOracle` are all `onlyOracle`. The oracle is currently the **rescue paper-key EOA** `0xCF884C81…082Eb`.
- `reth-rope-state-edit` — rewrites arbitrary account balances in MDBX out-of-band.

The safety of the *entire chain's balance integrity* now rests on: (a) the secrecy of one paper key, and (b) operational discipline that the state-edit binary is only ever run against a genuine `UntieRegistry` Tier-S declaration. There is no on-chain enforcement that a `state-edit` corresponds to a recorded, authorised untying — the binding is procedural. If the rescue key leaks (see M-2), an attacker can `rotateOracle` to themselves and mint a fully "audited-looking" recovery declaration for any theft they like. The primitive that saved 8.79B FAT can move 8.79B FAT.

Recommend (do not implement yet — for discussion): make `reth-rope-state-edit` refuse to apply a delta that does not match an on-chain `UntieRecord` whose `stateDeltaAppliedAt == 0`, and require the oracle to be a Safe/multisig, not an EOA.

---

## M-2 — Rescue wallet is a single paper-key EOA holding 8.79B FAT; Safe deadline cancelled

Per this session's decision, the hard 72-hour Safe-migration deadline was cancelled and the 8.79B FAT remains under a single paper-key EOA that also holds the `UntieRegistry` oracle role. This is a deliberate, documented, accepted risk (§4-quinquies), but it is a live single-point-of-failure of the same character as the original single-key deployer. The longer it persists, the more it resembles the pre-incident posture. It should not become permanent by inertia.

---

## M-3 — Mirrored trees multiply every leaked secret

`datachain-rope-p2b/`, `datachain-rope-deploy/`, and `dc-explorer-build-2026-03-07/` each carry their own copy of the leaked `.cursor/rules/` and/or source. That triples the number of places the compromised key lives, and they drift independently (e.g. `-p2b` has the key at line 177 vs 323 in the main tree). Any redaction of the primary tree that misses the mirrors leaves the key live. Decide whether these mirrors are still needed; if not, delete them; if yes, they must be included in every secret-hygiene sweep.

---

## L-1 — No guard prevents the next paste

There is no secret-scanning pre-commit hook or CI job in the workspace. The control that would have prevented 2026-06-22 — and would prevent its repeat — is a mechanical scan that rejects any commit/file containing a 64-hex-char private key or a `PRIVATE_KEY=` with a value. Its absence means the entire mitigation currently depends on human discipline, which already failed once.

---

## Recommended remediation order (for operator decision — nothing applied yet)

1. **[C-2] Rotate the deployer out of the minter set NOW** via the DCSwapTimelock: `setMinter(0x60FB32…, false)` on USDC/USDT/EUROD, and add a fresh hardware-key minter. This closes the unlimited-mint hole regardless of where the key text lives. (This is the single most urgent action — it is a live, higher-leverage version of the original breach.)
2. **[C-1/C-3/H-1] Purge + rotate:** treat `0x659f…63a88` as burned. Remove it from every file (main + 3 mirrors + deploy-scripts + `contracts_vps.env`), scrub git history (`git filter-repo`), and confirm nothing on-chain still depends on it. Convert deploy scripts to `process.env` with **no fallback default** and no production RPC default.
3. **[H-3] Turn Phase-2 signed-RPC ON** across the fleet once smoke-tested; it removes the reliance on the nginx-XFF heuristic.
4. **[M-1/M-2] Migrate the rescue balance + `UntieRegistry` oracle to a hardware Safe** and bind `reth-rope-state-edit` to an on-chain `UntieRecord`.
5. **[H-2] Sweep adjacent secrets** (Tanastok issuer, private-pool payout) into a secrets manager; forbid full keys in `.cursor/rules/`.
6. **[L-1] Add a secret-scanning pre-commit + CI gate** so the next paste is rejected mechanically.
7. **[M-3] Decide the fate of the mirrored trees**; delete or fold into the hygiene sweep.

---

*This document is findings-only. No remediation has been applied. On-chain-dependent claims are marked `[VERIFY ON-CHAIN]` and should be confirmed before acting, since the review terminal was unavailable at write time.*
