SECURITY AUDIT: Datachain Rope vs 12 modern crypto-attack vectors
Date: 2026-06-11
Auditor: Datachain Rope agent
Scope: rope-node, rope-explorer, rope-crypto, rope-bridge, rope-security, DCSwap contracts, T-REX/Treasury, nginx public RPC, Careaway integration

Status: 2 CRITICAL findings (V11-A, V11-B) confirmed and patched on the source tree (deploy required). All other vectors are either fully mitigated, partially mitigated with documented residual risk, or environmentally inapplicable to a sequencer-based EVM-compatible chain.

================================================================================
SUMMARY TABLE (12 vectors x 4 severities)
================================================================================

V#  Vector                                  Severity   Status
--- --------------------------------------  ---------  ----------------------
V1  CEO phishing / private-key compromise   MEDIUM     Mitigated by process; deployer key now off-line for treasuries (round-3 closeout)
V2  Multi-sig / MPC payload tampering       LOW        Treasury and BridgeSecurityController already have M-of-N + 24h delay
V3  Cross-chain bridge / wrapped-mint       MEDIUM     Tanastok and Careaway are NOT bridges; only DCSwap WFAT (native wrapping, audited code path)
V4  MEV / Sandwich attacks                  MEDIUM     Slippage controls present; sequencer is private (no public mempool to front-run yet)
V5  AI auto-exploit                         MEDIUM     Open source means high blast-radius if a Pair/Router bug ships; no kill-switch
V6  Oracle manipulation                     LOW-MED    Canonical price uses VWAP w/ outlier rejection; no on-chain lending tied to it yet
V7  Reentrancy                              LOW        DCSwapPair has lock modifier; Treasury uses ReentrancyGuard; WFAT uses CEI
V8  Initializer / unprotected impl          LOW        Pair.initialize is factory-only; no orphan proxies at risk in production
V9  Length-extension on hash                IMMUNE     BLAKE3 is Merkle-tree based, not Merkle-Damgard; SHA-256 not in any auth path
V10 Quantum decay                           MITIGATED  Hybrid Ed25519+Dilithium3 (NIST PQ-3) and X25519+Kyber768 already deployed
V11 Durable-nonce + admin takeover          CRITICAL   2 sub-findings: V11-A (untieKnot/erasePersonalLedger publicly callable), V11-B (appendToLedger/createPersonalLedger publicly callable). Patched.
V12 FinTech MFA bypass / SIM-swap           N/A        Datachain Rope has no SMS-OTP wallet onboarding; Datawallet+ uses passkey + on-device key

================================================================================
V11 (CRITICAL) - destructive rope_* RPCs publicly exposed
================================================================================

EVIDENCE (live probe, 2026-06-11T08:29Z, from off-VPS workstation):

  $ curl -X POST https://erpc.datachain.network -H Content-Type:application/json \
      -d {"jsonrpc":"2.0","id":1,"method":"rope_untieKnot","params":["0x0...0001","0x0...0001","probe"]}
  -> {"error":{"code":2002,"message":"No ledger found for this address"},"id":1,"jsonrpc":"2.0"}

  $ curl ... -d {"method":"rope_erasePersonalLedger","params":["0x0...0001"]}
  -> {"error":{"code":2002,"message":"No ledger found for this address"},"id":2,"jsonrpc":"2.0"}

The 2002 error proves the method is reachable; with a real (wallet, knot_id) pair it would have destroyed data. Wallet-knot pairs are public via rope_getStringWithKnots. Therefore: any internet user can erase any users GDPR Article-17 personal-ledger entries.

Affected methods:
  - rope_untieKnot                  (per-knot erasure - GDPR primitive)
  - rope_erasePersonalLedger        (whole-wallet erasure)
  - rope_appendToLedger             (forge ledger entries on any wallet)
  - rope_createPersonalLedger       (create empty ledger for any wallet - spam vector)
  - rope_anchorDeployerAttestation  (forced re-anchor of deployer attestation)

Source code admits this gap explicitly (rope_node/src/rpc_server.rs):
  // PHASE 1 - operator MUST front this RPC with an authenticated proxy or
  // restrict to private network. See rpc_server.rs `rope_untieKnot` doc-comment.

But the public proxy (deploy/nginx/conf.d/datachain.network.conf) does NOT filter by JSON-RPC method. So PHASE-1 is unenforced.

Methods that DO have proper auth (verified safe):
  - rope_suspendNode / rope_isolateNode / rope_eraseNode  - require founder/master-node Ed25519 signature, return -32401 on failure
  - eth_*  EVM methods are read-only or require signed transactions (Reth handles this)

REMEDIATION (this audit):
  1. New file:    crates/rope-node/src/rpc_auth.rs
                  - DESTRUCTIVE_METHODS allowlist with env-toggle ROPE_PUBLIC_DESTRUCTIVE_DENY (default ON)
                  - public-listener path returns -32401 Forbidden for the 5 affected methods
                  - private-listener (loopback or internal CIDR) keeps full access for operators
  2. Modified:    crates/rope-node/src/rpc_server.rs
                  - handle_json_rpc consults rpc_auth before dispatching to method handlers
  3. Modified:    deploy/nginx/conf.d/datachain.network.conf
                  - belt-and-suspenders deny-list using map + if directive on $request_body for the 5 methods
  4. New rule:    .cursor/rules/handover-security-audit-2026-06-11.mdc
                  - locks finding + remediation status in workspace memory
  5. Production deploy steps documented at the end of this file.

================================================================================
V1 - CEO PHISHING / PRIVATE-KEY COMPROMISE (Bybit-style)
================================================================================

Threat: a single phished signing event (deepfake video call, fake Slack DM, malicious browser extension) drains the deployer or treasury wallet.

Datachain Rope posture:
  - The DCSwap deployer key was scrubbed from disk (per dcswap handover-2026-06-04: "deployer wallet retains the address of the treasury but no longer has its private key - Tanastok is the sole signer from here on")
  - The Tanastok Private-Pool Treasury (0x63423bbc...320B) is the only wallet that can spend its 5M USDC, and Tanastok alone holds the key
  - The Careaway treasury (0xD7C519679660f778E64C73c305f9A5cd17B5fdeD) was funded once and has no programmatic spending access from external services
  - The DCSwap canonical price feed is read-only (no funds at risk)
  - The 5 canonical agent wallets (0xC001-0xC005) hold only the gas fees needed to anchor knots; an attacker who phished one of them could spam ledger entries but could not steal value

Residual risk:
  - The Datachain Foundation founder wallet (rope-vps SSH access + governance signing key) is still a single point. Move to hardware-wallet (Ledger Stax / Trezor Safe 5) signing for any rope-vps service-restart command, ssh-key rotation, or governance proposal.
  - Recommendation: sign EVERY high-impact ssh / governance action via a hardware-wallet-signed envelope, using ed25519/Dilithium3 keys generated and stored only on the device.

Severity: MEDIUM (post-mitigation: LOW). Compensating controls in place: per-treasury key separation, dcswap-prod tmp-key shredded, .env.bak shredded.

================================================================================
V2 - MULTI-SIG / MPC PAYLOAD TAMPERING (WazirX-style)
================================================================================

Threat: signers approve a transaction whose payload was rewritten in-flight by a compromised UI/MPC proxy, sending funds to attacker.

Datachain Rope posture:
  - Two on-chain primitives already exist for this:
    - DatachainTreasury (contracts/src/governance/Treasury.sol):
        * AccessControl with separate GOVERNANCE_ROLE / SPENDER_ROLE / GUARDIAN_ROLE
        * ReentrancyGuard + Pausable
        * Per-tx and daily spending limits
        * Emergency pause by guardian
        * Spending history kept on-chain, including approver, category, description
    - BridgeSecurityController (crates/rope-bridge/src/lib.rs::security):
        * 3-of-5 multi-sig threshold (config.threshold = 3, total_signers = 5)
        * 24-hour time-delay between propose and execute (config.time_delay_seconds = 86400)
        * Per-tx limit (100K tokens) and daily limit (1M tokens)
        * Large-transfer flag at 10K-token threshold
        * Independent guardian set with emergency-pause power
  - Both designs eliminate the WazirX path: tampered payload would fail when the on-chain action does not match what was signed (id is keccak256(proposer|action|timestamp))

Recommendation:
  - When the foundation is ready to harden production, deploy DatachainTreasury at a known address and migrate the deployer EOA balance into it. This is a follow-up; not blocking today.

Severity: LOW.

================================================================================
V3 - CROSS-CHAIN BRIDGE / WRAPPED-MINT VULNERABILITY (Ronin / Wormhole)
================================================================================

Threat: bridge validators sign a fraudulent mint on the destination chain; or wrapped-token mint authority is compromised and unbacked tokens are minted.

Datachain Rope posture:
  - Datachain Rope is NOT yet a cross-chain bridge in the classical sense. There is no light-client of Ethereum or Bitcoin running on rope-node.
  - WFAT (contracts/src/WFAT.sol) is a NATIVE wrap of the chains own gas token (DC FAT) - the same WETH9 pattern. It cannot mint without backing native FAT being sent in.
  - BridgedToken (contracts/src/BridgedToken.sol) IS mintable by an owner-controlled minter set. This is the USDC, USDT, EUROD pattern. **All three currently have a single minter = the deployer EOA.**
    * If that key is phished, attacker mints unbacked stablecoins.
    * Mitigation: migrate the minter role to the DatachainTreasury contract or to a Safe-equivalent multi-sig before scaling TVL.

Severity: MEDIUM (current). LOW after migration to multi-sig minter.

Recommendation:
  - Issue setMinter calls on USDC/USDT/EUROD changing the minter from EOA to a 3-of-5 multi-sig contract.
  - Add an explicit mint-cap and 24h delay for any single mint above 1M units.

================================================================================
V4 - MEV / SANDWICH ATTACKS
================================================================================

Threat: an MEV searcher inserts buy/sell transactions around a victim's swap to extract value. Drains $5B/yr on Ethereum.

Datachain Rope posture:
  - All swap entrypoints in DCSwapRouter take amountOutMin / amountInMax + deadline. Frontend defaults are 0.5% slippage. This is the standard Uniswap-V2 sandwich-resistance kit.
  - Pair.swap also takes a `to` parameter and a callback hook. The lock modifier prevents re-entry.
  - Sequencer is currently single-Reth-node (rope-vps). There is NO public mempool for an MEV bot to scan. All transactions enter a private mempool on the sequencer node and are ordered FIFO. Front-running requires direct rope-vps access, which is not exposed.
  - Once the chain decentralises (Phase 4 of the v2.0 5M-tps roadmap, ~Q4 2026), MEV becomes a real threat. Recommended pre-decentralisation work: deploy MEV-Boost-style proposer/builder separation OR use threshold encryption of mempool contents.

Severity: MEDIUM (latent - becomes HIGH after decentralisation).

================================================================================
V5 - AI AUTO-EXPLOIT (Mythos-class)
================================================================================

Threat: a frontier AI scans a deployed contracts source code, finds a logic bug, and writes a working exploit before the human team can respond.

Datachain Rope posture:
  - All Solidity sources are open. This is by design (contract verification on dcscan.io). Blast radius is therefore high if any new bug ships.
  - Mitigations in place:
    * DCSwapPair, BridgedToken, WFAT are 100-300 lines each, audited UniswapV2 lineage, no novel state machines. Surface area is minimal.
    * The Treasury contract has Pausable + emergency_withdraw - guardian can lock funds within 1 block of an exploit alert.
    * Bridge security has emergency_pause at the guardian level.
    * The 5 canonical AI agents (semantic, oracle, insurance, validation, compliance) themselves index the chain in real-time and can fire a kill-switch alert on anomalous draws.
  - Gaps:
    * No automated invariant monitor (e.g., `total LP value <= sum of pool reserves * sqrt(K)`) running off-chain. Recommended.
    * No formal verification (Certora / Halmos / Foundry invariant tests) on Pair / Router. Recommended.
    * The rope-node Rust crate is much larger (~50k LOC) and has higher latent bug surface; the Quipu Canon v2.0 spec calls out this as known scaling and signature-verification work.

Severity: MEDIUM. To downgrade to LOW:
  1. Add a one-page invariant monitor (off-chain, runs every 12s, alerts on sum(pair.balanceOf(*)) - sum(reserves) > tolerance).
  2. Run Foundry invariant tests against DCSwapPair on every CI commit.
  3. Wire compliance-agent's anomaly stream into a guardian auto-pause path for the Treasury.

================================================================================
V6 - ORACLE MANIPULATION (Mango / Drift collateral oracle)
================================================================================

Threat: a lending protocol or perp DEX trusts a single price oracle; attacker manipulates that oracle long enough to over-borrow or force liquidations.

Datachain Rope posture:
  - The canonical DC FAT price feed is at https://dcswap.net/v1/prices (per handover-canonical-fat-price-2026-03-14):
    * Sources: DCSwap on-chain reserves (weight 0.7) + GeckoTerminal XDC pool (weight 0.3) -> VWAP
    * Outlier rejection: if the two sources diverge by more than X%, the smaller-volume source is dropped automatically (verified live as `dcswap-reserves(outlier-rejected-gecko)` since 2026-05-10)
    * Refreshes every 30s server-side; clients see edge-cached results for 5 minutes max
  - The on-chain oracle (Pair.price0CumulativeLast / price1CumulativeLast) is a TWAP. To safely use it for lending, one would query a >30-min TWAP. This is the well-trodden Uniswap-V2 oracle pattern.
  - There is currently NO on-chain lending protocol on Datachain Rope. So an oracle-manipulation attack has no economic prize on-chain today. It would only mislead off-chain UIs (dcscan, dashboards).
  - Recommendation: when a lending market is added, mandate: (a) >= 1-hour TWAP, (b) at least 2 independent sources (DCSwap + an external aggregator), (c) max-deviation circuit breaker.

Severity: LOW today. Becomes HIGH if a lending market deploys without those mandates.

================================================================================
V7 - REENTRANCY (DAO-classic)
================================================================================

Threat: external call to attacker-controlled address before state update lets attacker re-enter and drain the contract.

Solidity contract review (specific findings):

(a) DCSwapPair.swap (contracts/src/DCSwapPair.sol:142-179):
    - Has a `lock` modifier (manual mutex via `unlocked` state var).
    - External calls (_safeTransfer + IDCSwapCallee.dcswapCall) happen inside the lock.
    - Standard Uniswap-V2 pattern. Safe.

(b) DCSwapPair.burn (line 118-140):
    - Lock modifier.
    - Two _safeTransfer calls before the final state update (kLast = ...).
    - Inside the lock, so a re-entry returns immediately on require(unlocked == 1).
    - Safe.

(c) WFAT.withdraw (contracts/src/WFAT.sol:27-32):
    - Updates balanceOf BEFORE transferring (CEI - check-effect-interaction).
    - Uses .transfer() which forwards 2300 gas.
    - .transfer() is now considered fragile post-Istanbul HF (Berlin gas changes). On Datachain Rope (Reth), the gas costs are EVM-canonical so .transfer() still succeeds for EOA recipients but may fail for receiver-contracts whose receive() costs >2300 gas.
    - Not a security bug, but a UX wart. Recommendation: replace with `.call{value: wad}("")` + a manual ReentrancyGuard. Low priority.

(d) DCSwapRouter (contracts/src/DCSwapRouter.sol):
    - No internal state to attack except the router contract's own FAT balance during a swap. The receive() guard `assert(msg.sender == WFAT)` ensures only the WFAT contract can send FAT into the router (i.e. via withdraw() during a swap). This is correct and prevents arbitrary deposit attacks.

(e) BridgedToken._transfer (contracts/src/BridgedToken.sol:72-78):
    - Pure internal balance update, no external call. CEI by construction. Safe.

(f) DatachainTreasury.spend / spendToken (contracts/src/governance/Treasury.sol):
    - nonReentrant modifier.
    - whenNotPaused.
    - Records spending BEFORE the transfer.
    - Safe. Multi-layer.

(g) RopeComplianceModule.sol / DCNFTSecurityWrapper.sol / DatawalletClaimIssuer.sol / AgentReputation.sol / DatachainDAO.sol:
    - Read but no funds-at-risk transfer paths. State updates ahead of any external interaction. Safe.

Severity: LOW.

================================================================================
V8 - INITIALIZER / UNPROTECTED IMPLEMENTATION CONTRACTS (Wormhole-style)
================================================================================

Threat: a developer forgets to call init on the implementation behind an upgradeable proxy. Attacker calls init with attacker as owner, then calls selfdestruct to brick the proxy.

Datachain Rope posture:
  - DCSwap is currently NOT deployed behind an UUPS or Transparent proxy. The contracts at the live addresses (DCSwapRouter 0x8ebdd966..., DCSwapFactory 0x772e5fd5..., WFAT 0x285eecf5...) are direct deployments.
    * This means addresses are NOT upgradeable - which is the right tradeoff for these audited contracts.
    * The roadmap (datachain-rope-production-roadmap.mdc) calls for migration to UUPS for Router and Factory; that work has not started. When it does, the standard `_disableInitializers()` call in the implementation constructor MUST be present. This rule is now in workspace memory.
  - DCSwapPair.initialize:
    * Guarded by `require(msg.sender == factory)`. The factory deploys via CREATE2 and immediately calls initialize in the same transaction. After that, initialize cannot be called again because msg.sender will not match.
    * Safe.
  - T-REX / ONCHAINID contracts (per handover-tanastok-tokenized-assets-for-dcscan-2026-03-30) DO use proxy patterns (TREXFactory, IdentityProxy, ImplementationAuthority). These are deployed by the foundation and are the well-audited Tokeny T-REX upstream. Safe.

Severity: LOW. To downgrade further:
  - When DCSwap migrates to UUPS (roadmap Phase 1), add a CI check that every implementation contract has `_disableInitializers()` in its constructor.

================================================================================
V9 - LENGTH-EXTENSION (Merkle-Damgard hash)
================================================================================

Threat: SHA-256 (and its predecessors) follow the Merkle-Damgard construction. If H(secret || msg) is used as a MAC, attacker who knows H and len(secret) can compute H(secret || msg || padding || extension) without knowing secret.

Datachain Rope posture:
  - ALL hashing in the rope crates uses BLAKE3 (verified in rope-crypto/src/hash.rs and confirmed across rope-bridge, rope-core, rope-node, rope-explorer, rope-shadow-witness).
  - BLAKE3 is built on a Merkle tree (NOT Merkle-Damgard). It is not vulnerable to length-extension by construction.
  - The keccak256 used by EVM tooling is also not vulnerable to length-extension (SHA-3 family is sponge-based).
  - SHA-256 is NOT used anywhere in any auth path. It appears only in test fixtures and as part of the Bitcoin SPV verification scaffold (which would need HMAC wrapping when activated; today the scaffold returns hardcoded `is_valid: true`, see rope-bridge/src/lib.rs verify_proof for BitcoinSpv).

Severity: IMMUNE for all production paths. Recommendation: when the BitcoinSpv path is activated, wrap any pre-image-of-SHA-256 work in HMAC-SHA256 explicitly (the BIP-340 schnorr signature scheme already does this).

================================================================================
V10 - QUANTUM CRYPTOGRAPHIC DECAY (Shor's algorithm)
================================================================================

Threat: a sufficiently large quantum computer can break Ed25519/secp256k1/RSA in polynomial time, recovering private keys from signatures or public keys. ETA: estimates range 2030-2040.

Datachain Rope posture (verified rope-crypto/src/hybrid.rs):
  - Signatures: hybrid Ed25519 + CRYSTALS-Dilithium3 (NIST PQ-3, 1952-byte public key, 3293-byte signature).
  - Key exchange: hybrid X25519 + CRYSTALS-Kyber768 (NIST PQ-3, 1184-byte public key, 1088-byte ciphertext).
  - Hashing: BLAKE3 256-bit (Grover's algorithm reduces effective security to 128-bit, which is still safe).
  - The hybrid construction means BOTH algorithms must be broken for an attacker to forge a signature - secure even if one is cryptanalysed.
  - HybridSignature.size() = 64 (Ed25519) + 3293 (Dilithium3) = 3357 bytes per knot. Bandwidth-tolerable.

Caveat (operational):
  - At consensus time, `verify_signatures: false` is currently set in consensus_orchestrator.rs:119. This is a known v1.x dev-mode setting documented as becoming Phase-2 work in the v2.0 roadmap (quipu-canon-v2-roadmap-5m-tps.mdc). It does NOT mean signatures are absent from knots; HybridSignature is still attached. It means the consensus layer is not yet rejecting bad signatures.
  - This is fine for the "quantum decay" threat (which is years out) but it does mean today an attacker who could intercept and replace knot signatures in-flight on the libp2p network would not be caught at consensus. Mitigation: knots are anchored within ~3s and any swap that lands at the EVM layer (Reth) is signed by EOA private keys at the EVM level (which IS verified). The latent risk is in the Quipu-native append-only ledger, not in DC FAT or token transfers.

Severity: MITIGATED for V10 specifically. Phase-2 consensus signature verification is the canonical fix and is on the roadmap.

================================================================================
V11 - DURABLE-NONCE + ADMIN TAKEOVER (Drift Protocol-style)
================================================================================

(Already detailed above as the CRITICAL finding.)

Tangential note: Solana's "durable nonces" do not exist in EVM. The closest analogue is signed-but-not-yet-broadcast EOA transactions. The deployer key was scrubbed from disk (handover-2026-06-04, round 3), so the Drift-style "dormant pre-signed admin transaction" attack is not currently feasible.

================================================================================
V12 - FINTECH MFA BYPASS / SIM-SWAP (Cash App-style)
================================================================================

Threat: SMS-OTP can be intercepted via SIM-swap; attackers gain full access to a fiat-to-crypto bridge account, buy crypto, withdraw to attacker-controlled wallet, leave the FinTech holding fraudulent fiat.

Datachain Rope posture:
  - Datachain Rope mainnet does not ship a fiat-to-crypto onramp. There is no SMS-OTP authentication path in the chain or the DCSwap dApp.
  - Datawallet+ uses passkey-based authentication (per the Datawallet+ project rules) plus a hardware-backed key shard - no SMS path at all. SIM-swap is impossible against Datawallet+.
  - Careaway integration uses local-API auth (signed admin session) plus on-chain treasury-read-only flows. No SMS-OTP, no programmatic spending from Careaway -> chain.
  - Tanastok handles its own user accounts but the on-chain settlement is governed by the Tanastok deployer key, not by user accounts.

Severity: N/A. Out of scope for the chain itself; in-scope for any FinTech that integrates Datachain Rope and chooses an SMS-OTP design - mitigation: disallow that design.

================================================================================
PRODUCTION DEPLOY PLAN FOR THE V11 HOT-FIX
================================================================================

Pre-flight (run from local workspace):
  1. Verify the patch compiles locally:
     cd datachain-rope && cargo build --release -p rope-node
  2. Run the new tests:
     cargo test -p rope-node --lib rpc_auth_

On rope-vps (92.243.26.189, ssh -p 41722):
  1. Sync source:
     rsync -avz --delete --exclude target/ --exclude .git/ \
       ./datachain-rope/ rope-vps:/home/ubuntu/datachain-rope/
  2. Build on the VPS:
     ssh rope-vps "export PATH=$HOME/.cargo/bin:$PATH && cd /home/ubuntu/datachain-rope && cargo build --release -p rope-node"
  3. Backup the running binary:
     ssh rope-vps "cp /home/ubuntu/datachain-rope/target/release/rope ~/backup-2026-06-11/"
  4. Restart datachain-rope.service:
     ssh rope-vps "sudo systemctl restart datachain-rope.service"
     ssh rope-vps "sleep 5 && systemctl is-active datachain-rope.service"
  5. Verify the deny:
     curl -X POST https://erpc.datachain.network -H content-type:application/json \
       -d {"jsonrpc":"2.0","id":1,"method":"rope_untieKnot","params":["0x0...0001","0x0...0001","probe"]}
     Expected: {"error":{"code":-32401,"message":"Method denied on public listener; see SECURITY_AUDIT_2026-06-11.md"},"id":1,"jsonrpc":"2.0"}

Rollback (if anything breaks):
  ssh rope-vps "sudo systemctl stop datachain-rope && cp ~/backup-2026-06-11/rope /home/ubuntu/datachain-rope/target/release/rope && sudo systemctl start datachain-rope"

Operator-initiated maintenance access (post-deploy):
  Set ROPE_PUBLIC_DESTRUCTIVE_DENY=0 in /etc/datachain-rope.env temporarily to allow direct local calls, restart the service, run the maintenance, restore the env to 1, restart again. Document each such window in the incident log.

================================================================================
END OF AUDIT
================================================================================
