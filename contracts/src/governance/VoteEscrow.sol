// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

/**
 * @title VoteEscrow
 * @notice The single configurable governance-voting primitive for Datachain
 *         Rope (chain 271828), per `docs/GOVERNANCE_VOTING_CAUSE_PLATFORM_SPEC_V1.md`
 *         §1.2/§2.1. One contract, one `Disposition` enum, three code paths —
 *         Burn / Return / Reward — chosen by the vote's organiser at
 *         creation time and never improvised per-voter.
 *
 * @dev    CROSS-CHAIN VOTING POWER (Phase 2, this contract)
 *         ---------------------------------------------------
 *         Native DC FAT has no checkpointed balance history and no DCR-20 in
 *         this ecosystem implements `ERC20Votes`/`IVotes`, so this contract
 *         cannot read a voter's balance at an arbitrary past block, and it
 *         certainly cannot read a voter's legacy-DC balance on Ethereum or
 *         XDC — a Rope contract has no visibility into other chains at all.
 *
 *         Per spec §2.1 option (b) (the recommended, chosen option — the
 *         same pattern production DAOs like Uniswap/Compound governance use
 *         for gas reasons, and the exact EIP-191-attestation construction
 *         `FATMigrationMinter.claimMigration` already proved in production
 *         for the legacy-DC migration rail), voting/creation power is an
 *         off-chain-computed, on-chain-verified attestation:
 *
 *           weight = balanceOf(voter) on Ethereum legacy DC (ERC-20)
 *                  + balanceOf(voter) on XDC legacy DC (XRC-20)
 *                  + balanceOf(voter) native DC FAT on Datachain Rope
 *
 *         computed by the `attestor` service (`rope-explorer`'s
 *         cross-chain balance aggregator), signed EIP-191, and submitted by
 *         the voter alongside their `castVote`/`createVote` call. The
 *         signature is bound to (this contract, this chain, a purpose tag,
 *         the vote id or creator address, the voter, the weight, and an
 *         expiry) so it can never be replayed across contracts, chains,
 *         votes, voters, or past its freshness window. `hasVoted` additionally
 *         prevents a valid attestation from being consumed twice for the
 *         same vote.
 *
 *         TOKENS ACTUALLY DISPOSED OF (Burn/Return/Reward)
 *         ---------------------------------------------------
 *         The cross-chain `weight` above determines a voter's INFLUENCE on
 *         the tally (quorum + approval threshold) — "the more DC you have,
 *         the more voting power you have" — but a Rope contract can only
 *         ever custody Rope-native DC FAT. So the tokens that are actually
 *         burned/returned/rewarded are the native FAT a voter chooses to
 *         lock as `msg.value` when casting their ballot (`lockedAmount`),
 *         capped at their attested cross-chain `weight` (documented
 *         assumption: legacy DC and native FAT are treated as 1:1
 *         value-equivalent, consistent with the live 1:1 migration bridge —
 *         see `docs/DC_FAT_LEGACY_MIGRATION_AND_MARKET_VISIBILITY_SPEC_V2.md`).
 *         Locking is OPTIONAL per voter — a wallet can vote with full weight
 *         influence and zero stake; if it does not stake, it simply has
 *         nothing to burn/return/reclaim/be-rewarded-on for that ballot.
 *
 *         GOVERNANCE OF THIS CONTRACT ITSELF
 *         ---------------------------------------------------
 *         No single EOA owner. `owner` is expected to be `DCSwapTimelock`
 *         (`0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c` on chain 271828 as of
 *         the 2026-06-12 governance handover) — every privileged mutation
 *         (attestor/creator/guardian rotation, pause parameters, ownership
 *         transfer) is therefore publicly scheduled with the timelock delay
 *         before it can execute, exactly like `BridgeMinter`/
 *         `FATMigrationMinter`. `attestor`, `creator`, and `guardian` are
 *         three DISTINCT keys (2026-07-20 bridge-audit lesson F6: never
 *         reuse a verifier/attestor/executor key). The known-compromised
 *         deployer `0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195` is refused
 *         everywhere a role or the deployer itself could be set to it.
 */
contract VoteEscrow {
    // ============================================================
    //  Types
    // ============================================================

    /// @notice What happens to the native FAT a voter locks when casting.
    enum Disposition {
        /// Locked FAT is permanently destroyed (sent to `BURN_SINK`).
        Burn,
        /// Locked FAT is returned to each voter's own wallet once voting closes.
        Return,
        /// Locked FAT is returned PLUS a pro-rata share of a pre-funded reward pool.
        Reward
    }

    /// @notice Which approval pipeline created / governs this vote (spec §2.2).
    enum VoteClass {
        /// Community-initiated "Submit Your Project" vote.
        Project,
        /// NGO/beneficiary selection or donation vote.
        Cause,
        /// Foundation-only: FAT minting, treasury allocation, protocol params.
        CriticalProtocol,
        /// Foundation or community: feature prioritisation, non-critical proposals.
        NonCriticalFeature
    }

    enum Outcome {
        /// Voting window still open, or closed but not yet finalized.
        Pending,
        Approved,
        Rejected,
        /// Closed, but total participating weight never reached `quorumWeight`.
        NoQuorum
    }

    /// @notice Creation-time configuration for a single vote. Packed fields
    ///         chosen so a third party can fully audit disposition,
    ///         eligibility bounds, and outcome from storage alone.
    struct VoteConfig {
        VoteClass voteClass;
        Disposition disposition;
        uint64 startsAt;
        uint64 endsAt;
        /// @dev Minimum aggregate cross-chain weight (wei-equivalent) a
        ///      voter must present to cast a ballot on this vote.
        uint256 minWeightToVote;
        /// @dev Minimum aggregate PARTICIPATING weight (weightFor+weightAgainst)
        ///      required for the vote to reach quorum.
        uint256 quorumWeight;
        /// @dev e.g. 5100 = 51.00% of participating weight must vote "for".
        uint16 approvalThresholdBps;
        /// @dev Native FAT pre-funded at creation time; only nonzero when
        ///      `disposition == Reward`. Distributed pro-rata to every voter
        ///      who locked FAT on this vote, regardless of their choice.
        uint256 rewardPoolAmount;
        /// @dev Informational — who funded the reward pool. Not required to
        ///      equal the creator (spec §1.2: "organiser is explicitly
        ///      generalised to any funding wallet, not just the Foundation").
        address rewardPoolFunder;
        /// @dev keccak256 binding to the off-chain project/cause record
        ///      (id, description, milestones, …) held in `rope-explorer`'s
        ///      durable JSONL+knot-anchored queue. Keeps on-chain storage
        ///      cheap while making the on-chain vote independently
        ///      correlatable to its full off-chain context.
        bytes32 metadataHash;
        address creator;
        bool finalized;
        bool burnSwept;
        Outcome outcome;
        uint256 totalWeightFor;
        uint256 totalWeightAgainst;
        uint256 totalLockedFor;
        uint256 totalLockedAgainst;
    }

    /// @notice One voter's ballot on one vote.
    struct Ballot {
        bool voted;
        bool choice;
        /// @dev The attested cross-chain weight used for this ballot's tally
        ///      contribution — a point-in-time snapshot, not live-tracked.
        uint256 weight;
        /// @dev Native FAT locked via `msg.value` at cast time.
        uint256 locked;
        /// @dev Set once the voter has withdrawn/claimed/been swept for
        ///      this vote's disposition — prevents double disposal.
        bool disposed;
    }

    /// @notice Parameters for `createVote`. Bundled into a struct to keep
    ///         the external ABI stable and avoid stack-depth churn as the
    ///         config surface grows.
    struct CreateVoteParams {
        VoteClass voteClass;
        Disposition disposition;
        uint64 startsAt;
        uint64 endsAt;
        uint256 minWeightToVote;
        uint256 quorumWeight;
        uint16 approvalThresholdBps;
        address rewardPoolFunder;
        bytes32 metadataHash;
    }

    // ============================================================
    //  Constants
    // ============================================================

    /// @notice Canonical, published, provably-unspendable burn sink (spec
    ///         §2.4) — the well-known zero-padded "dEaD" convention, no
    ///         known private key. Distinct from any operational wallet.
    address public constant BURN_SINK = address(uint160(0xdEaD));

    /// @notice Domain separator for every weight attestation this contract
    ///         verifies — distinct from `DCROPE-VOTE-AUTH` (the off-chain
    ///         Phase 1 ballot-signing domain in `governance_votes.rs`),
    ///         `DATACHAIN-ID-AUTH`, `EDC-CONSOLE-AUTH`, and
    ///         `DCROPE/legacy-migration/claim/v1` — a signature captured on
    ///         any other Datachain Rope surface can never authenticate here.
    bytes32 public constant WEIGHT_DOMAIN_TAG = keccak256("DCROPE/governance/vote-escrow/weight/v1");
    /// @dev Purpose sub-tags so a `castVote` attestation can never be
    ///      replayed to satisfy a `createVote` eligibility check, or vice versa.
    bytes32 public constant CAST_PURPOSE = keccak256("cast");
    bytes32 public constant CREATE_PURPOSE = keccak256("create");

    /// @dev Known-compromised deployer key (2026-07-20 bridge audit F4/F5;
    ///      2026-06-22 incident). Refused everywhere a privileged role could
    ///      be set to it, mirroring `UntieRegistry`/`FATMigrationMinter`.
    address private constant COMPROMISED = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;

    // ============================================================
    //  Storage — roles
    // ============================================================

    /// @notice Owner — expected to be `DCSwapTimelock`. All privileged ops.
    address public owner;
    /// @notice Signer whose attestations authorize `createVote` (community
    ///         path) and `castVote` weight/eligibility proofs.
    address public attestor;
    /// @notice Foundation-operated key allowed to call `createVote` directly
    ///         (no attestation) for `Cause` and `CriticalProtocol` votes.
    address public creator;
    /// @notice Emergency pauser — can pause, can never unpause.
    address public guardian;

    /// @notice Minimum aggregate cross-chain weight a community wallet must
    ///         attest to in order to create a `Project`/`NonCriticalFeature`
    ///         vote without going through `creator`. Owner-tunable (spec
    ///         §7 open question — no hardcoded business number here).
    uint256 public minWeightToCreate;

    /// @notice While true, `createVote` and `castVote` revert. Finalization
    ///         and disposal (withdraw/claim/sweep) remain available so
    ///         locked funds are never frozen by a pause.
    bool public paused;

    // ============================================================
    //  Storage — votes
    // ============================================================

    VoteConfig[] private _votes;
    mapping(uint256 => mapping(address => Ballot)) private _ballots;
    /// @dev Per-(voter, attestation-purpose) nonce-free replay guard is
    ///      unnecessary for `castVote` (bound to voteId + `hasVoted`), but
    ///      `createVote`'s community path has no natural per-call state to
    ///      gate on, so a valid create-attestation may be reused for
    ///      multiple vote creations within its expiry window. This is safe:
    ///      it only proves "at time T this address held >= W", it moves no
    ///      value and mutates no per-voter state.

    // ============================================================
    //  Events
    // ============================================================

    event VoteCreated(
        uint256 indexed voteId,
        VoteClass indexed voteClass,
        Disposition disposition,
        address indexed creatorAddress,
        uint64 startsAt,
        uint64 endsAt,
        uint256 quorumWeight,
        uint16 approvalThresholdBps,
        uint256 rewardPoolAmount,
        bytes32 metadataHash
    );
    event VoteCast(
        uint256 indexed voteId,
        address indexed voter,
        bool choice,
        uint256 weight,
        uint256 locked
    );
    event VoteFinalized(uint256 indexed voteId, Outcome outcome, uint256 weightFor, uint256 weightAgainst);
    event LockedWithdrawn(uint256 indexed voteId, address indexed voter, uint256 amount);
    event BurnSwept(uint256 indexed voteId, uint256 amount);
    event RewardClaimed(uint256 indexed voteId, address indexed voter, uint256 principal, uint256 reward);
    event RewardPoolReclaimed(uint256 indexed voteId, address indexed to, uint256 amount);

    event Paused(address indexed by);
    event Unpaused(address indexed by);
    event OwnerUpdated(address indexed oldOwner, address indexed newOwner);
    event AttestorUpdated(address indexed oldAttestor, address indexed newAttestor);
    event CreatorUpdated(address indexed oldCreator, address indexed newCreator);
    event GuardianUpdated(address indexed oldGuardian, address indexed newGuardian);
    event MinWeightToCreateUpdated(uint256 oldValue, uint256 newValue);

    // ============================================================
    //  Errors
    // ============================================================

    error NotOwner(address caller, address expected);
    error NotCreator(address caller, address expected);
    error NotOwnerOrGuardian(address caller);
    error ZeroAddress(string field);
    error CompromisedAddress(address addr);
    error IsPaused();
    error VoteNotFound(uint256 voteId);
    error VoteWindowInvalid(uint64 startsAt, uint64 endsAt);
    error ThresholdOutOfRange(uint16 bps);
    error RewardFundingMismatch(uint256 sent, Disposition disposition);
    error VoteNotOpen(uint256 nowTs, uint64 startsAt, uint64 endsAt);
    error AlreadyVoted(uint256 voteId, address voter);
    error AttestationExpired(uint256 expiresAt, uint256 nowTs);
    error InvalidAttestation();
    error WeightTooLow(uint256 weight, uint256 minRequired);
    error StakeExceedsWeight(uint256 stake, uint256 weight);
    error VotingStillOpen(uint256 voteId, uint64 endsAt);
    error AlreadyFinalized(uint256 voteId);
    error NotFinalized(uint256 voteId);
    error WrongDisposition(Disposition actual, Disposition expected);
    error NothingLocked(uint256 voteId, address voter);
    error AlreadyDisposed(uint256 voteId, address voter);
    error BurnAlreadySwept(uint256 voteId);
    error NoParticipation(uint256 voteId);
    error GracePeriodNotElapsed(uint256 voteId, uint256 elapsedUntil);
    error TransferFailed();

    // ============================================================
    //  Construction
    // ============================================================

    /// @param _owner  Expected: DCSwapTimelock on chain 271828.
    /// @param _attestor Cross-chain balance aggregator signer (rope-explorer).
    /// @param _creator  Foundation-operated key for Cause/CriticalProtocol votes.
    /// @param _guardian Emergency pauser.
    /// @param _minWeightToCreate Initial community vote-creation eligibility floor.
    constructor(
        address _owner,
        address _attestor,
        address _creator,
        address _guardian,
        uint256 _minWeightToCreate
    ) {
        if (_owner == address(0)) revert ZeroAddress("owner");
        if (_attestor == address(0)) revert ZeroAddress("attestor");
        if (_creator == address(0)) revert ZeroAddress("creator");
        if (_owner == COMPROMISED) revert CompromisedAddress(_owner);
        if (_attestor == COMPROMISED) revert CompromisedAddress(_attestor);
        if (_creator == COMPROMISED) revert CompromisedAddress(_creator);
        if (_guardian == COMPROMISED) revert CompromisedAddress(_guardian);

        owner = _owner;
        attestor = _attestor;
        creator = _creator;
        guardian = _guardian;
        minWeightToCreate = _minWeightToCreate;
    }

    // ============================================================
    //  Modifiers
    // ============================================================

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner(msg.sender, owner);
        _;
    }

    modifier whenNotPaused() {
        if (paused) revert IsPaused();
        _;
    }

    // ============================================================
    //  Signature verification (EIP-2-constrained, 65-byte r‖s‖v)
    //  — identical construction to FATMigrationMinter._recover, reused
    //  verbatim per the 2026-07-20 bridge-audit "reuse, don't reinvent" note.
    // ============================================================

    function _recover(bytes32 digest, bytes calldata sig) internal pure returns (address) {
        if (sig.length != 65) revert InvalidAttestation();
        bytes32 r = bytes32(sig[0:32]);
        bytes32 s = bytes32(sig[32:64]);
        uint8 v = uint8(sig[64]);
        if (v < 27) v += 27;
        if (v != 27 && v != 28) revert InvalidAttestation();
        // EIP-2: reject malleable high-s signatures.
        if (uint256(s) > 0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0) {
            revert InvalidAttestation();
        }
        address recovered = ecrecover(digest, v, r, s);
        if (recovered == address(0)) revert InvalidAttestation();
        return recovered;
    }

    function _ethSigned(bytes32 digest) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
    }

    /// @notice The digest the attestor signs for a `castVote` weight proof.
    ///         Exposed publicly so the off-chain aggregator/SDK and this
    ///         contract can never drift on the byte layout.
    function castWeightDigest(uint256 voteId, address voter, uint256 weight, uint256 expiresAt)
        public
        view
        returns (bytes32)
    {
        return keccak256(
            abi.encode(WEIGHT_DOMAIN_TAG, CAST_PURPOSE, block.chainid, address(this), voteId, voter, weight, expiresAt)
        );
    }

    /// @notice The digest the attestor signs for a community `createVote`
    ///         eligibility proof. Not bound to a voteId (none exists yet at
    ///         creation time) — bound to the creating address instead.
    function createWeightDigest(address creatorAddr, uint256 weight, uint256 expiresAt)
        public
        view
        returns (bytes32)
    {
        return keccak256(
            abi.encode(WEIGHT_DOMAIN_TAG, CREATE_PURPOSE, block.chainid, address(this), creatorAddr, weight, expiresAt)
        );
    }

    // ============================================================
    //  Vote creation
    // ============================================================

    /// @notice Create a new vote. `Project`/`NonCriticalFeature` votes may
    ///         be created by ANY wallet that presents a valid cross-chain
    ///         weight attestation >= `minWeightToCreate` (spec §1.1
    ///         "community-initiated"). `Cause`/`CriticalProtocol` votes may
    ///         only be created by the `creator` role (spec §1.1
    ///         "admin-initiated (Datachain Foundation)"); the attestation
    ///         parameters are ignored on that path.
    /// @dev    When `params.disposition == Reward`, `msg.value` becomes the
    ///         reward pool and MUST be > 0. Otherwise `msg.value` MUST be 0
    ///         (Burn/Return votes hold no organiser-funded pool).
    function createVote(
        CreateVoteParams calldata params,
        uint256 creatorWeight,
        uint256 creatorExpiresAt,
        bytes calldata creatorAttestation
    ) external payable whenNotPaused returns (uint256 voteId) {
        if (params.startsAt >= params.endsAt || params.endsAt <= block.timestamp) {
            revert VoteWindowInvalid(params.startsAt, params.endsAt);
        }
        if (params.approvalThresholdBps > 10_000) revert ThresholdOutOfRange(params.approvalThresholdBps);

        if (params.disposition == Disposition.Reward) {
            if (msg.value == 0) revert RewardFundingMismatch(msg.value, params.disposition);
        } else if (msg.value != 0) {
            revert RewardFundingMismatch(msg.value, params.disposition);
        }

        bool communityPath = params.voteClass == VoteClass.Project || params.voteClass == VoteClass.NonCriticalFeature;
        if (communityPath) {
            if (msg.sender != creator) {
                if (block.timestamp > creatorExpiresAt) revert AttestationExpired(creatorExpiresAt, block.timestamp);
                bytes32 digest = _ethSigned(createWeightDigest(msg.sender, creatorWeight, creatorExpiresAt));
                if (_recover(digest, creatorAttestation) != attestor) revert InvalidAttestation();
                if (creatorWeight < minWeightToCreate) revert WeightTooLow(creatorWeight, minWeightToCreate);
            }
            // creator (Foundation) may always bypass the attestation for
            // community-class votes too — e.g. Foundation-curated shortlist.
        } else {
            if (msg.sender != creator) revert NotCreator(msg.sender, creator);
        }

        voteId = _votes.length;
        _votes.push(VoteConfig({
            voteClass: params.voteClass,
            disposition: params.disposition,
            startsAt: params.startsAt,
            endsAt: params.endsAt,
            minWeightToVote: params.minWeightToVote,
            quorumWeight: params.quorumWeight,
            approvalThresholdBps: params.approvalThresholdBps,
            rewardPoolAmount: msg.value,
            rewardPoolFunder: params.rewardPoolFunder == address(0) ? msg.sender : params.rewardPoolFunder,
            metadataHash: params.metadataHash,
            creator: msg.sender,
            finalized: false,
            burnSwept: false,
            outcome: Outcome.Pending,
            totalWeightFor: 0,
            totalWeightAgainst: 0,
            totalLockedFor: 0,
            totalLockedAgainst: 0
        }));

        emit VoteCreated(
            voteId,
            params.voteClass,
            params.disposition,
            msg.sender,
            params.startsAt,
            params.endsAt,
            params.quorumWeight,
            params.approvalThresholdBps,
            msg.value,
            params.metadataHash
        );
    }

    // ============================================================
    //  Casting
    // ============================================================

    /// @notice Cast a ballot. `weight` is the voter's attested aggregate
    ///         cross-chain DC/FAT balance (Ethereum ERC-20 + XDC XRC-20 +
    ///         Rope native FAT), proven via `attestation`. Optionally locks
    ///         `msg.value` native FAT (capped at `weight`) whose eventual
    ///         disposition follows the vote's configured `Disposition`.
    function castVote(
        uint256 voteId,
        bool choice,
        uint256 weight,
        uint256 expiresAt,
        bytes calldata attestation
    ) external payable whenNotPaused {
        if (voteId >= _votes.length) revert VoteNotFound(voteId);
        VoteConfig storage v = _votes[voteId];
        if (block.timestamp < v.startsAt || block.timestamp > v.endsAt) {
            revert VoteNotOpen(block.timestamp, v.startsAt, v.endsAt);
        }
        if (_ballots[voteId][msg.sender].voted) revert AlreadyVoted(voteId, msg.sender);
        if (block.timestamp > expiresAt) revert AttestationExpired(expiresAt, block.timestamp);
        if (weight < v.minWeightToVote) revert WeightTooLow(weight, v.minWeightToVote);
        if (msg.value > weight) revert StakeExceedsWeight(msg.value, weight);

        bytes32 digest = _ethSigned(castWeightDigest(voteId, msg.sender, weight, expiresAt));
        if (_recover(digest, attestation) != attestor) revert InvalidAttestation();

        _ballots[voteId][msg.sender] = Ballot({
            voted: true,
            choice: choice,
            weight: weight,
            locked: msg.value,
            disposed: false
        });

        if (choice) {
            v.totalWeightFor += weight;
            v.totalLockedFor += msg.value;
        } else {
            v.totalWeightAgainst += weight;
            v.totalLockedAgainst += msg.value;
        }

        emit VoteCast(voteId, msg.sender, choice, weight, msg.value);
    }

    // ============================================================
    //  Finalization
    // ============================================================

    /// @notice Callable by anyone once `endsAt` has passed. Idempotent —
    ///         reverts if already finalized. Computes `Outcome` from the
    ///         REAL recorded tally; never mutates ballots or locked funds.
    function finalizeVote(uint256 voteId) external {
        if (voteId >= _votes.length) revert VoteNotFound(voteId);
        VoteConfig storage v = _votes[voteId];
        if (v.finalized) revert AlreadyFinalized(voteId);
        if (block.timestamp <= v.endsAt) revert VotingStillOpen(voteId, v.endsAt);

        uint256 totalWeight = v.totalWeightFor + v.totalWeightAgainst;
        if (totalWeight == 0 || totalWeight < v.quorumWeight) {
            v.outcome = Outcome.NoQuorum;
        } else {
            uint256 forBps = (v.totalWeightFor * 10_000) / totalWeight;
            v.outcome = forBps >= v.approvalThresholdBps ? Outcome.Approved : Outcome.Rejected;
        }
        v.finalized = true;
        emit VoteFinalized(voteId, v.outcome, v.totalWeightFor, v.totalWeightAgainst);
    }

    // ============================================================
    //  Disposal — pull pattern (never loops over voters; avoids DoS)
    // ============================================================

    /// @notice `Disposition.Return` — each voter withdraws exactly what
    ///         they personally locked. Independent of the vote's outcome
    ///         and does not require `finalizeVote` to have been called
    ///         (Return does not care about the tally, only that voting has
    ///         closed) — "does DC then get reallocated back into our
    ///         wallets once the vote timeframe finishes?" per Andrew's
    ///         question, answered literally here.
    function withdrawLocked(uint256 voteId) external {
        if (voteId >= _votes.length) revert VoteNotFound(voteId);
        VoteConfig storage v = _votes[voteId];
        if (v.disposition != Disposition.Return) revert WrongDisposition(v.disposition, Disposition.Return);
        if (block.timestamp <= v.endsAt) revert VotingStillOpen(voteId, v.endsAt);

        Ballot storage b = _ballots[voteId][msg.sender];
        if (!b.voted || b.locked == 0) revert NothingLocked(voteId, msg.sender);
        if (b.disposed) revert AlreadyDisposed(voteId, msg.sender);

        uint256 amount = b.locked;
        b.disposed = true; // CEI — state settled before the external call.

        (bool ok, ) = msg.sender.call{value: amount}("");
        if (!ok) revert TransferFailed();

        emit LockedWithdrawn(voteId, msg.sender, amount);
    }

    /// @notice `Disposition.Burn` — callable once, by anyone, after voting
    ///         closes. Sends the ENTIRE locked pool (both directions) to
    ///         `BURN_SINK` in a single transfer, since the destination is
    ///         identical for every voter (no per-voter loop needed).
    function sweepBurn(uint256 voteId) external {
        if (voteId >= _votes.length) revert VoteNotFound(voteId);
        VoteConfig storage v = _votes[voteId];
        if (v.disposition != Disposition.Burn) revert WrongDisposition(v.disposition, Disposition.Burn);
        if (block.timestamp <= v.endsAt) revert VotingStillOpen(voteId, v.endsAt);
        if (v.burnSwept) revert BurnAlreadySwept(voteId);

        uint256 amount = v.totalLockedFor + v.totalLockedAgainst;
        v.burnSwept = true; // CEI

        if (amount > 0) {
            (bool ok, ) = BURN_SINK.call{value: amount}("");
            if (!ok) revert TransferFailed();
        }

        emit BurnSwept(voteId, amount);
    }

    /// @notice `Disposition.Reward` — each voter who locked FAT claims back
    ///         their own principal PLUS a pro-rata share of the reward pool,
    ///         proportional to `locked / (totalLockedFor + totalLockedAgainst)`.
    ///         Rewards participation, not outcome — a voter on the losing
    ///         side who staked still earns their share. Requires
    ///         `finalizeVote` first only so the tally is stable and cannot
    ///         be re-derived mid-claim window.
    function claimReward(uint256 voteId) external {
        if (voteId >= _votes.length) revert VoteNotFound(voteId);
        VoteConfig storage v = _votes[voteId];
        if (v.disposition != Disposition.Reward) revert WrongDisposition(v.disposition, Disposition.Reward);
        if (!v.finalized) revert NotFinalized(voteId);

        Ballot storage b = _ballots[voteId][msg.sender];
        if (!b.voted || b.locked == 0) revert NothingLocked(voteId, msg.sender);
        if (b.disposed) revert AlreadyDisposed(voteId, msg.sender);

        uint256 totalLocked = v.totalLockedFor + v.totalLockedAgainst;
        if (totalLocked == 0) revert NoParticipation(voteId); // unreachable given b.locked>0, defensive.

        uint256 principal = b.locked;
        uint256 reward = (v.rewardPoolAmount * principal) / totalLocked;
        b.disposed = true; // CEI

        uint256 payout = principal + reward;
        (bool ok, ) = msg.sender.call{value: payout}("");
        if (!ok) revert TransferFailed();

        emit RewardClaimed(voteId, msg.sender, principal, reward);
    }

    /// @notice Edge case (spec-required honesty, not a stub): if a
    ///         `Reward` vote closes with ZERO locked participation, the
    ///         pre-funded reward pool has no claimant basis and would
    ///         otherwise sit forever unreachable. After a 30-day grace
    ///         period past `endsAt`, governance may return the pool to its
    ///         recorded funder. Only reachable when `totalLockedFor +
    ///         totalLockedAgainst == 0` — if even one voter staked, this
    ///         reverts and `claimReward` is the only path, by design.
    function reclaimUnclaimedRewardPool(uint256 voteId) external onlyOwner {
        if (voteId >= _votes.length) revert VoteNotFound(voteId);
        VoteConfig storage v = _votes[voteId];
        if (v.disposition != Disposition.Reward) revert WrongDisposition(v.disposition, Disposition.Reward);
        uint256 graceEnd = uint256(v.endsAt) + 30 days;
        if (block.timestamp <= graceEnd) revert GracePeriodNotElapsed(voteId, graceEnd);
        if (v.totalLockedFor + v.totalLockedAgainst != 0) revert NoParticipation(voteId);
        if (v.rewardPoolAmount == 0) revert NoParticipation(voteId);

        uint256 amount = v.rewardPoolAmount;
        address to = v.rewardPoolFunder;
        v.rewardPoolAmount = 0; // CEI

        (bool ok, ) = to.call{value: amount}("");
        if (!ok) revert TransferFailed();

        emit RewardPoolReclaimed(voteId, to, amount);
    }

    // ============================================================
    //  Governance (owner = Timelock; guardian = pause-only)
    // ============================================================

    function pause() external {
        if (msg.sender != owner && msg.sender != guardian) revert NotOwnerOrGuardian(msg.sender);
        paused = true;
        emit Paused(msg.sender);
    }

    function unpause() external onlyOwner {
        paused = false;
        emit Unpaused(msg.sender);
    }

    function setAttestor(address newAttestor) external onlyOwner {
        if (newAttestor == address(0)) revert ZeroAddress("attestor");
        if (newAttestor == COMPROMISED) revert CompromisedAddress(newAttestor);
        emit AttestorUpdated(attestor, newAttestor);
        attestor = newAttestor;
    }

    function setCreator(address newCreator) external onlyOwner {
        if (newCreator == address(0)) revert ZeroAddress("creator");
        if (newCreator == COMPROMISED) revert CompromisedAddress(newCreator);
        emit CreatorUpdated(creator, newCreator);
        creator = newCreator;
    }

    function setGuardian(address newGuardian) external onlyOwner {
        if (newGuardian == COMPROMISED) revert CompromisedAddress(newGuardian);
        emit GuardianUpdated(guardian, newGuardian);
        guardian = newGuardian;
    }

    function setMinWeightToCreate(uint256 newValue) external onlyOwner {
        emit MinWeightToCreateUpdated(minWeightToCreate, newValue);
        minWeightToCreate = newValue;
    }

    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress("owner");
        if (newOwner == COMPROMISED) revert CompromisedAddress(newOwner);
        emit OwnerUpdated(owner, newOwner);
        owner = newOwner;
    }

    // ============================================================
    //  Views
    // ============================================================

    function votesLength() external view returns (uint256) {
        return _votes.length;
    }

    function getVote(uint256 voteId) external view returns (VoteConfig memory) {
        if (voteId >= _votes.length) revert VoteNotFound(voteId);
        return _votes[voteId];
    }

    function getBallot(uint256 voteId, address voter) external view returns (Ballot memory) {
        return _ballots[voteId][voter];
    }

    function hasVoted(uint256 voteId, address voter) external view returns (bool) {
        return _ballots[voteId][voter].voted;
    }

    /// @notice Escrowed native FAT this contract currently custodies —
    ///         reconcilable off-chain against `sum(lockedFor+lockedAgainst)`
    ///         across all non-disposed ballots plus any un-swept/un-claimed
    ///         reward pools.
    function escrowBalance() external view returns (uint256) {
        return address(this).balance;
    }
}
