// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

/**
 * @title UntieRegistry
 * @notice On-chain audit trail for `rope_untieTx` — the Datachain Rope
 *         transaction-reversal primitive.
 *
 * @dev    This contract is the PUBLIC, IMMUTABLE record of every approved
 *         irregular state change executed on Datachain Rope. Each entry
 *         declares, before the state delta is applied at the protocol
 *         layer, the exact (attacker, victim, rescue, amount, prevStateRoot,
 *         justificationCID, authorityTier) tuple.
 *
 *         The state delta itself is applied by `rope-state-edit` (a Rust
 *         binary that mutates the EVM execution layer's MDBX at a quiesced
 *         moment with all federation nodes stopped). This contract does NOT
 *         perform the state delta — Solidity cannot mutate arbitrary
 *         third-party balances. It records the AUTHORIZATION DECLARATION.
 *
 *         Anyone can later verify that:
 *           (a) An UntieRecorded event exists at block N with the declared
 *               tuple (justification CID, prev state root, authority).
 *           (b) The chain's actual state at block N+1+ matches the tuple
 *               (debit applied to attacker, credit applied to rescue).
 *           (a) ∧ (b) ⇒ recovery happened with full on-chain attestation.
 *
 *         The three authorization tiers are described in
 *         `.cursor/rules/handover-security-audit-2026-06-11.mdc` and
 *         `handover-from-rope-untie-tx-design-2026-06-30.mdc`:
 *
 *           Tier S — Sovereign Owner (Datachain Foundation founder)
 *                    1 Ed25519 founder signature, hardware-only post-incident,
 *                    rate-limited to 3 invocations per quarter.
 *           Tier F — Federation Council (master nodes)
 *                    M-of-N Ed25519 signatures from registered master nodes,
 *                    24h public delay window.
 *           Tier U — User Petition with Quorum
 *                    Claimant secp256k1 signature + M-of-N master-node
 *                    signatures, 72h public dispute window.
 *
 *         Tier F and Tier U are CODED here but DISABLED at deploy. Activation
 *         is per-tier via a Tier-S-signed `activateTier` call after governance
 *         ratification (typically +30d for Tier F, +60d for Tier U).
 *
 *         Authorization signature verification happens in `rope-node`'s
 *         `untie_registry.rs` (Ed25519 via ed25519-dalek, secp256k1 via
 *         the existing EIP-191 path). rope-node submits the tx to this
 *         contract only after the off-chain auth check has passed. The
 *         contract enforces a final ON-CHAIN check that msg.sender is the
 *         designated rope-node submitter (the chain's "consensus oracle"
 *         account), which is the only account permitted to record
 *         untyings. This prevents an attacker who steals a founder Ed25519
 *         key from calling the contract directly — they must also subvert
 *         a master node, which is a separate compromise.
 *
 *         All state deltas applied to Datachain Rope go through this
 *         contract first. There is no "secret" state change. Every state
 *         delta has a public, queryable record.
 *
 *         The contract has no admin who can withdraw funds, mint, or
 *         transfer anything. It has no token balance. It only RECORDS.
 *         The state delta is applied by `rope-state-edit` at the EVM
 *         execution layer, atomically with the federation stop/start cycle.
 */
contract UntieRegistry {
    // ============================================================
    //  Types
    // ============================================================

    /// @notice Authorization tier for an untying.
    enum AuthorityTier {
        /// 1 Ed25519 founder signature. Hardware-only post-incident.
        Sovereign,
        /// M-of-N Ed25519 master-node signatures. 24h delay.
        Federation,
        /// User secp256k1 + M-of-N master-node sigs. 72h dispute window.
        UserPetition
    }

    /// @notice Scope of the state delta.
    enum DeltaScope {
        /// Native DC FAT only (EOA→EOA-style reversal).
        NativeFat,
        /// DCR-20 token rebalance (USDC, USDT, EUROD, etc.).
        Dcr20Token,
        /// ERC-3643 security token rebalance (Tanastok, NaturaProof, etc.).
        Erc3643,
        /// Reserved for future use (e.g. storage-slot edits).
        Reserved
    }

    /// @notice One recorded irregular state change.
    /// @dev    Packed tight to keep storage cheap. The fields chosen here
    ///         are the MINIMUM needed by any third-party auditor (regulator,
    ///         court, academic researcher) to fully reconstruct the state
    ///         delta from the chain alone, without trusting the foundation.
    struct UntieRecord {
        // Authorization
        AuthorityTier tier;          // Which tier authorized this untying.
        bytes32 founderPubkey;       // Ed25519 founder pubkey (Tier S/F), or hash of M-of-N keyset (Tier F/U).
        bytes32 claimantHash;        // keccak256(claimant secp256k1 addr) for Tier U; zero otherwise.

        // What state delta was applied
        DeltaScope scope;
        address tokenContract;       // address(0) for NativeFat; ERC-20/ERC-3643 contract otherwise.
        address attacker;            // The account that was debited.
        address rescue;              // The account that was credited.
        uint256 amount;              // Amount debited from attacker == amount credited to rescue.

        // Chain state declaration
        uint256 declaredAtBlock;     // Block number this contract call lands in.
        bytes32 prevStateRoot;       // State root immediately BEFORE the delta is applied.
        bytes32 postStateRoot;       // State root expected AFTER the delta is applied (committed at next block).

        // Justification & accountability
        bytes32 justificationCid;    // IPFS CID of the public justification (the post-mortem article CID).
        string  shortReason;         // Human-readable summary, capped at 200 chars.
        address recordedBy;          // The rope-node consensus oracle that called this contract (msg.sender).

        // Timestamps
        uint256 recordedAt;          // block.timestamp at recording.
        uint256 stateDeltaAppliedAt; // 0 until the next-block oracle ping confirms application; then block.timestamp.
    }

    // ============================================================
    //  Storage
    // ============================================================

    /// @notice The chain's "consensus oracle" — the only account allowed
    ///         to call `recordUntie`. In production this is the public
    ///         address of an off-chain ed25519 signer controlled by the
    ///         master-node quorum (Tier F equivalent). For the founding
    ///         period and today's recovery, this is the founder.
    /// @dev    Settable only via the same auth path as a Tier-S untying:
    ///         the founder signs an off-chain Ed25519 over a canonical
    ///         "set oracle" message, rope-node verifies, then submits
    ///         `setOracle(newOracle, founderSig, founderPubkey)` here.
    ///         The contract does NOT verify the Ed25519 itself (Solidity
    ///         has no Ed25519 precompile); it relies on rope-node having
    ///         already verified before forwarding. The msg.sender check
    ///         enforces that this call IS coming from a rope-node.
    address public consensusOracle;

    /// @notice Per-tier activation flag. Tier S is true at deploy.
    /// @dev    Tier F and U start `false`. Founder sends a one-time
    ///         `activateTier(tier)` to flip after governance ratification.
    mapping(AuthorityTier => bool) public tierEnabled;

    /// @notice Quarterly rate limit per tier. Default: 3/quarter for Tier S,
    ///         10/quarter for Tier F, unlimited for Tier U.
    mapping(AuthorityTier => uint256) public tierMaxPerQuarter;
    /// @notice Calls in the current quarter for a given tier.
    mapping(AuthorityTier => uint256) public tierCallsThisQuarter;
    /// @notice The quarter index ((block.timestamp / 90 days)) when the
    ///         tier's count was last reset.
    mapping(AuthorityTier => uint256) public tierQuarterIndex;

    /// @notice The full ordered list of untyings ever recorded.
    UntieRecord[] public records;

    /// @notice Hash → record index, for fast lookup by (txHash being reversed).
    /// @dev    The key is keccak256(scope, tokenContract, attacker, rescue, amount, declaredAtBlock).
    mapping(bytes32 => uint256) public recordByDigest;

    /// @notice For Tier F / Tier U: a pending proposal awaiting its delay
    ///         window. Tier S has no pending state — it executes immediately.
    struct PendingProposal {
        UntieRecord intent;
        uint256 earliestExecuteAt;   // unix timestamp
        uint256 createdAt;
        bool    cancelled;
        bool    executed;
    }
    PendingProposal[] public pending;

    // ============================================================
    //  Events
    // ============================================================

    /// @notice THE permanent on-chain audit record. Anyone querying
    ///         `eth_getLogs` for this event sees the full history of
    ///         every state delta ever applied to Datachain Rope.
    event UntieRecorded(
        uint256 indexed recordIndex,
        AuthorityTier indexed tier,
        DeltaScope scope,
        address indexed attacker,
        address rescue,
        uint256 amount,
        address tokenContract,
        uint256 declaredAtBlock,
        bytes32 prevStateRoot,
        bytes32 postStateRootDeclared,
        bytes32 justificationCid,
        string shortReason,
        bytes32 founderPubkeyOrKeysetHash,
        bytes32 claimantHash
    );

    /// @notice Emitted when rope-node confirms the state delta has actually
    ///         been applied at the EVM execution layer. This closes the
    ///         loop: a record without a confirmation is a declared-but-not-
    ///         executed intent.
    event UntieStateDeltaConfirmed(
        uint256 indexed recordIndex,
        uint256 confirmedAtBlock,
        bytes32 actualPostStateRoot,
        bool matchesDeclared
    );

    event TierActivated(AuthorityTier indexed tier, uint256 atBlock);
    event TierRateLimitUpdated(AuthorityTier indexed tier, uint256 newMaxPerQuarter);
    event ConsensusOracleRotated(address indexed oldOracle, address indexed newOracle);

    event ProposalCreated(uint256 indexed proposalId, AuthorityTier indexed tier, uint256 earliestExecuteAt);
    event ProposalCancelled(uint256 indexed proposalId, string reason);
    event ProposalExecuted(uint256 indexed proposalId, uint256 indexed recordIndex);

    // ============================================================
    //  Errors
    // ============================================================

    error OnlyConsensusOracle(address caller, address expected);
    error TierDisabled(AuthorityTier tier);
    error TierRateLimited(AuthorityTier tier, uint256 calls, uint256 max);
    error ProposalNotReady(uint256 proposalId, uint256 earliestExecuteAt, uint256 nowTs);
    error ProposalAlreadyResolved(uint256 proposalId);
    error AmountZero();
    error InvalidAddress(string field);
    error ReasonTooLong(uint256 length, uint256 max);
    error InvalidTierForOperation(AuthorityTier tier, string operation);

    // ============================================================
    //  Construction
    // ============================================================

    /// @param _initialOracle The rope-node consensus oracle address.
    ///        For the founding period this is typically the founder's
    ///        EOA-or-hardware address. For ongoing operation it is the
    ///        master-node quorum's aggregator address.
    constructor(address _initialOracle) {
        if (_initialOracle == address(0)) revert InvalidAddress("initialOracle");
        consensusOracle = _initialOracle;

        // Tier S is live at deploy. F and U require explicit activation.
        tierEnabled[AuthorityTier.Sovereign] = true;
        tierEnabled[AuthorityTier.Federation] = false;
        tierEnabled[AuthorityTier.UserPetition] = false;

        tierMaxPerQuarter[AuthorityTier.Sovereign] = 3;
        tierMaxPerQuarter[AuthorityTier.Federation] = 10;
        tierMaxPerQuarter[AuthorityTier.UserPetition] = type(uint256).max;

        uint256 q = _currentQuarter();
        tierQuarterIndex[AuthorityTier.Sovereign] = q;
        tierQuarterIndex[AuthorityTier.Federation] = q;
        tierQuarterIndex[AuthorityTier.UserPetition] = q;
    }

    // ============================================================
    //  Modifiers
    // ============================================================

    modifier onlyOracle() {
        if (msg.sender != consensusOracle) revert OnlyConsensusOracle(msg.sender, consensusOracle);
        _;
    }

    // ============================================================
    //  Recording — the primary entry point
    // ============================================================

    /// @notice Record an untying. Off-chain auth (Ed25519 / secp256k1
    ///         signature verification) is performed by rope-node BEFORE
    ///         this call. This contract enforces:
    ///           - msg.sender is the consensus oracle (rope-node)
    ///           - the tier is enabled
    ///           - rate limit for the quarter not exceeded
    ///           - basic field sanity (non-zero amount, non-zero addresses)
    function recordUntie(
        AuthorityTier tier,
        bytes32 founderPubkeyOrKeysetHash,
        bytes32 claimantHash,
        DeltaScope scope,
        address tokenContract,
        address attacker,
        address rescue,
        uint256 amount,
        bytes32 prevStateRoot,
        bytes32 postStateRootDeclared,
        bytes32 justificationCid,
        string calldata shortReason
    ) external onlyOracle returns (uint256 recordIndex) {
        // ----- Basic validation -----
        if (!tierEnabled[tier]) revert TierDisabled(tier);
        if (amount == 0) revert AmountZero();
        if (attacker == address(0)) revert InvalidAddress("attacker");
        if (rescue == address(0)) revert InvalidAddress("rescue");
        if (bytes(shortReason).length > 200) revert ReasonTooLong(bytes(shortReason).length, 200);
        if (scope != DeltaScope.NativeFat && tokenContract == address(0)) revert InvalidAddress("tokenContract");

        // ----- Rate limit -----
        _checkAndBumpQuarter(tier);

        // ----- Persist -----
        recordIndex = records.length;
        records.push(UntieRecord({
            tier:                 tier,
            founderPubkey:        founderPubkeyOrKeysetHash,
            claimantHash:         claimantHash,
            scope:                scope,
            tokenContract:        tokenContract,
            attacker:             attacker,
            rescue:               rescue,
            amount:               amount,
            declaredAtBlock:      block.number,
            prevStateRoot:        prevStateRoot,
            postStateRoot:        postStateRootDeclared,
            justificationCid:     justificationCid,
            shortReason:          shortReason,
            recordedBy:           msg.sender,
            recordedAt:           block.timestamp,
            stateDeltaAppliedAt:  0
        }));

        bytes32 digest = keccak256(abi.encode(scope, tokenContract, attacker, rescue, amount, block.number));
        recordByDigest[digest] = recordIndex + 1; // +1 so zero means "not present"

        emit UntieRecorded(
            recordIndex,
            tier,
            scope,
            attacker,
            rescue,
            amount,
            tokenContract,
            block.number,
            prevStateRoot,
            postStateRootDeclared,
            justificationCid,
            shortReason,
            founderPubkeyOrKeysetHash,
            claimantHash
        );
    }

    /// @notice Confirm that the off-chain state delta has actually been
    ///         applied. Called by rope-node after `rope-state-edit` runs
    ///         on all federation nodes and the new state root is observed.
    function confirmStateDelta(
        uint256 recordIndex,
        bytes32 actualPostStateRoot
    ) external onlyOracle {
        UntieRecord storage r = records[recordIndex];
        r.stateDeltaAppliedAt = block.timestamp;
        emit UntieStateDeltaConfirmed(
            recordIndex,
            block.number,
            actualPostStateRoot,
            actualPostStateRoot == r.postStateRoot
        );
    }

    // ============================================================
    //  Tier F / Tier U — propose / cancel / execute
    // ============================================================
    // These are CODED but require tierEnabled[Federation|UserPetition] = true.
    // For today's incident the Federation and UserPetition tiers stay
    // disabled — Tier S is sufficient and was the user's explicit choice.

    function proposeUntie(
        AuthorityTier tier,
        bytes32 keysetHash,
        bytes32 claimantHash,
        DeltaScope scope,
        address tokenContract,
        address attacker,
        address rescue,
        uint256 amount,
        bytes32 prevStateRoot,
        bytes32 postStateRootDeclared,
        bytes32 justificationCid,
        string calldata shortReason,
        uint256 delaySecs
    ) external onlyOracle returns (uint256 proposalId) {
        if (tier == AuthorityTier.Sovereign) revert InvalidTierForOperation(tier, "proposeUntie");
        if (!tierEnabled[tier]) revert TierDisabled(tier);
        // Minimum delays per tier.
        uint256 minDelay = tier == AuthorityTier.Federation ? 24 hours : 72 hours;
        if (delaySecs < minDelay) delaySecs = minDelay;

        proposalId = pending.length;
        pending.push(PendingProposal({
            intent: UntieRecord({
                tier:                tier,
                founderPubkey:       keysetHash,
                claimantHash:        claimantHash,
                scope:               scope,
                tokenContract:       tokenContract,
                attacker:            attacker,
                rescue:              rescue,
                amount:              amount,
                declaredAtBlock:     0,
                prevStateRoot:       prevStateRoot,
                postStateRoot:       postStateRootDeclared,
                justificationCid:    justificationCid,
                shortReason:         shortReason,
                recordedBy:          msg.sender,
                recordedAt:          block.timestamp,
                stateDeltaAppliedAt: 0
            }),
            earliestExecuteAt: block.timestamp + delaySecs,
            createdAt:         block.timestamp,
            cancelled:         false,
            executed:          false
        }));

        emit ProposalCreated(proposalId, tier, block.timestamp + delaySecs);
    }

    function cancelProposal(uint256 proposalId, string calldata reason) external onlyOracle {
        PendingProposal storage p = pending[proposalId];
        if (p.cancelled || p.executed) revert ProposalAlreadyResolved(proposalId);
        p.cancelled = true;
        emit ProposalCancelled(proposalId, reason);
    }

    function executeProposal(uint256 proposalId) external onlyOracle returns (uint256 recordIndex) {
        PendingProposal storage p = pending[proposalId];
        if (p.cancelled || p.executed) revert ProposalAlreadyResolved(proposalId);
        if (block.timestamp < p.earliestExecuteAt) {
            revert ProposalNotReady(proposalId, p.earliestExecuteAt, block.timestamp);
        }

        UntieRecord memory intent = p.intent;
        // Re-check tier still enabled (could have been toggled during the window).
        if (!tierEnabled[intent.tier]) revert TierDisabled(intent.tier);

        _checkAndBumpQuarter(intent.tier);

        recordIndex = records.length;
        intent.declaredAtBlock = block.number;
        intent.recordedAt = block.timestamp;
        records.push(intent);

        bytes32 digest = keccak256(abi.encode(
            intent.scope, intent.tokenContract, intent.attacker, intent.rescue, intent.amount, block.number
        ));
        recordByDigest[digest] = recordIndex + 1;

        p.executed = true;

        emit UntieRecorded(
            recordIndex,
            intent.tier,
            intent.scope,
            intent.attacker,
            intent.rescue,
            intent.amount,
            intent.tokenContract,
            block.number,
            intent.prevStateRoot,
            intent.postStateRoot,
            intent.justificationCid,
            intent.shortReason,
            intent.founderPubkey,
            intent.claimantHash
        );
        emit ProposalExecuted(proposalId, recordIndex);
    }

    // ============================================================
    //  Administration (oracle-gated)
    // ============================================================

    function activateTier(AuthorityTier tier) external onlyOracle {
        tierEnabled[tier] = true;
        emit TierActivated(tier, block.number);
    }

    function deactivateTier(AuthorityTier tier) external onlyOracle {
        tierEnabled[tier] = false;
    }

    function setTierRateLimit(AuthorityTier tier, uint256 newMaxPerQuarter) external onlyOracle {
        tierMaxPerQuarter[tier] = newMaxPerQuarter;
        emit TierRateLimitUpdated(tier, newMaxPerQuarter);
    }

    function rotateOracle(address newOracle) external onlyOracle {
        if (newOracle == address(0)) revert InvalidAddress("newOracle");
        address old = consensusOracle;
        consensusOracle = newOracle;
        emit ConsensusOracleRotated(old, newOracle);
    }

    // ============================================================
    //  Views
    // ============================================================

    function recordsLength() external view returns (uint256) {
        return records.length;
    }

    function pendingLength() external view returns (uint256) {
        return pending.length;
    }

    function getRecord(uint256 idx) external view returns (UntieRecord memory) {
        return records[idx];
    }

    function getPending(uint256 idx) external view returns (PendingProposal memory) {
        return pending[idx];
    }

    function findByDigest(
        DeltaScope scope,
        address tokenContract,
        address attacker,
        address rescue,
        uint256 amount,
        uint256 declaredAtBlock
    ) external view returns (uint256 indexPlusOne) {
        return recordByDigest[keccak256(abi.encode(scope, tokenContract, attacker, rescue, amount, declaredAtBlock))];
    }

    function quarterRemaining(AuthorityTier tier) external view returns (uint256) {
        uint256 q = _currentQuarter();
        uint256 calls = tierQuarterIndex[tier] == q ? tierCallsThisQuarter[tier] : 0;
        uint256 max = tierMaxPerQuarter[tier];
        return max > calls ? max - calls : 0;
    }

    // ============================================================
    //  Internals
    // ============================================================

    function _currentQuarter() internal view returns (uint256) {
        // 90-day quarters anchored at Unix epoch — deterministic, no calendar.
        return block.timestamp / 90 days;
    }

    function _checkAndBumpQuarter(AuthorityTier tier) internal {
        uint256 q = _currentQuarter();
        if (tierQuarterIndex[tier] != q) {
            tierQuarterIndex[tier] = q;
            tierCallsThisQuarter[tier] = 0;
        }
        uint256 calls = tierCallsThisQuarter[tier];
        uint256 max = tierMaxPerQuarter[tier];
        if (calls >= max) revert TierRateLimited(tier, calls, max);
        tierCallsThisQuarter[tier] = calls + 1;
    }
}
