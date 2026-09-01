// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/access/AccessControl.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/utils/SafeERC20.sol";
import "@openzeppelin/contracts/utils/ReentrancyGuard.sol";
import "@openzeppelin/contracts/utils/Pausable.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "@openzeppelin/contracts/utils/cryptography/EIP712.sol";

/**
 * @title MapstoreEscrow
 * @notice Trustless DCR-20 stablecoin escrow for Mapstore service jobs and orders
 * @dev Deployed on Datachain Rope (Chain ID: 271828). Settles `ServiceJob` and
 *      multi-merchant basket payments from the Mapstore marketplace
 *      (https://github.com/mapstore/mapstore - `mapstore-core::ServiceJob`,
 *      `mapstore-core::Order`).
 *
 * Lifecycle (mirrors `mapstore-core::ServiceJobStatus`):
 *
 *   Pending  -> openJob(buyer, payee, token, amount, jobId)
 *               buyer.transfer(amount) into this contract
 *   Matched  -> assignPayee(jobId, payee)         (optional, when matched late)
 *   InProgress -> startJob(jobId)                   (timer for auto-release starts)
 *   Completed -> releaseJob(jobId)                  (payee receives funds, minus fee)
 *   Disputed -> openDispute(jobId)                  (only buyer or payee in window)
 *               -> resolveDispute(jobId, payeeAmt, buyerAmt)   (operator only)
 *   Cancelled -> refundJob(jobId)                   (before InProgress, buyer-only)
 *
 * Key properties:
 * - **Atomic open**: buyer's funds enter escrow in the same tx as the job creation.
 *   No "I paid but the job never showed up" race.
 * - **Time-bounded**: every InProgress job has a `disputeDeadline`. If neither party
 *   raises a dispute by then, anyone can call `autoRelease(jobId)` to push funds to
 *   the payee. This avoids platform float and removes the "Mapstore-held escrow"
 *   problem from FUNCTIONAL_AND_TECHNICAL_SPEC.md section 12.2.
 * - **Platform fee taken at release**: Mapstore's commission is configurable and
 *   deducted at `releaseJob` time, transparently. Default 8% matches
 *   `MerchantPricing.commission_pct`.
 * - **Operator can resolve disputes**: a multisig-controlled `OPERATOR_ROLE` can
 *   split escrow funds between buyer and payee after a dispute window. The
 *   resolution is public and append-only.
 * - **GDPR-friendly metadata**: jobs reference `bytes32 metadataHash` (a BLAKE3 or
 *   keccak256 hash of the Mapstore-side job descriptor). The contract never stores
 *   shopper PII or order line items - everything stays in the off-chain job record,
 *   only the hash sits on chain. When the shopper requests erasure under GDPR
 *   Art. 17, the off-chain bytes get untied via `rope_untieKnot`; the hash on this
 *   contract remains as a tombstone reference, with no recoverable payload.
 *
 * Token support: any DCR-20 fungible token. Mapstore defaults to USDC
 * (0xb93bd8db94f1baff474aa9cba0739daaad01641f). USDT, EUROD and future
 * Mapstore-pinned stables work without redeploy because the token address is
 * captured per-job.
 *
 * SECURITY (2026-07-26 counter-audit fix, "PLATFORM_ROLE can pull funds and
 * settle without buyer"): the "relayer model" documented above previously
 * relied entirely on an OFF-CHAIN claim ("the buyer's off-chain approve +
 * signed intent are validated on the Mapstore side before this call") as the
 * sole authorization for {openJob}, {assignPayee}, and {releaseJob} when
 * called by PLATFORM_ROLE on a buyer's behalf. A compromised or malicious
 * PLATFORM_ROLE key could therefore, using nothing but a victim's *pre-existing*
 * DCR-20 approval to this contract: (1) open an unwanted job against the
 * victim with an attacker-controlled `payee`, (2) reassign an existing job's
 * `payee` to an attacker address, and (3) force-release escrowed funds to
 * that payee — with zero on-chain evidence the buyer ever consented to that
 * specific job. Every one of those three entry points now additionally
 * requires a fresh EIP-712 signature from the affected `buyer` (via
 * {_requireBuyerAuthorization}) whenever the caller is NOT the buyer
 * themselves. The signature is scoped to a `deadline` and to the exact
 * parameters of the action (job id, payee, token, amount, fee), so a
 * compromised relayer key can no longer originate, redirect, or settle a job
 * without the buyer's real, on-chain-verifiable consent for that action.
 */
contract MapstoreEscrow is AccessControl, ReentrancyGuard, Pausable, EIP712 {
    using SafeERC20 for IERC20;
    using ECDSA for bytes32;

    // -- Roles -------------------------------------------------------------

    /// @notice Mapstore platform signer. Authorises job creation and release
    ///         on behalf of buyers and payees via EIP-191 signatures so a
    ///         buyer never has to pay gas - the Mapstore relayer does.
    bytes32 public constant PLATFORM_ROLE = keccak256("PLATFORM_ROLE");

    /// @notice Multi-sig that resolves disputes when buyer and payee disagree.
    ///         In production this points at the Mapstore DAO Safe; in pilots
    ///         it points at the Mapstore operator EOA.
    bytes32 public constant OPERATOR_ROLE = keccak256("OPERATOR_ROLE");

    /// @notice Emergency pauser. Same multisig pattern as `Treasury.sol`.
    bytes32 public constant GUARDIAN_ROLE = keccak256("GUARDIAN_ROLE");

    // -- Types -------------------------------------------------------------

    enum JobStatus {
        None,         // 0 - never created (default for unknown ids)
        Pending,      // 1 - opened, awaiting acceptance
        InProgress,   // 2 - work started, dispute window open
        Completed,    // 3 - funds released to payee
        Disputed,     // 4 - dispute raised, operator must resolve
        Resolved,     // 5 - operator split funds, terminal
        Cancelled     // 6 - buyer refunded before InProgress, terminal
    }

    struct Job {
        bytes32 id;
        address buyer;
        address payee;
        IERC20  token;
        uint256 amount;
        uint256 platformFeeBps; // basis points (8% = 800)
        bytes32 metadataHash;   // off-chain Mapstore job descriptor hash
        uint64  openedAt;
        uint64  startedAt;
        uint64  disputeDeadline;
        JobStatus status;
    }

    // -- Storage -----------------------------------------------------------

    /// @notice Job by id. Ids are deterministic on the Mapstore side
    ///         (`keccak256(bytes(job.id))`) so reconciliation is one round-trip.
    mapping(bytes32 => Job) public jobs;

    /// @notice Platform fee recipient. Mapstore treasury wallet.
    address public platformTreasury;

    /// @notice Default platform fee in basis points. Per-job override is allowed.
    ///         800 = 8% to match `MerchantPricing.commission_pct` default.
    uint256 public defaultPlatformFeeBps;

    /// @notice Dispute window after `startJob`. Default 7 days. After this
    ///         elapses without a dispute, anyone can `autoRelease` the job.
    uint256 public defaultDisputeWindow;

    /// @notice Hard cap on the fee anyone can set on a single job. Prevents
    ///         a compromised PLATFORM_ROLE from draining a job by setting a
    ///         99% fee. 2000 = 20% absolute ceiling.
    uint256 public constant MAX_PLATFORM_FEE_BPS = 2000;

    /// @notice Hard cap on dispute window (prevents indefinite locking).
    uint256 public constant MAX_DISPUTE_WINDOW = 90 days;

    /// @notice Hard floor on dispute window so operators cannot squeeze it
    ///         below the time a real human needs to react.
    uint256 public constant MIN_DISPUTE_WINDOW = 1 hours;

    // -- EIP-712 buyer authorization (2026-07-26 counter-audit fix) --------

    /// @dev Signed by `buyer` off-chain, submitted by the PLATFORM_ROLE
    ///      relayer alongside {openJob}. Binds the exact job parameters so a
    ///      compromised relayer cannot originate an unwanted job.
    bytes32 private constant OPEN_JOB_TYPEHASH = keccak256(
        "OpenJobAuthorization(bytes32 id,address payee,address token,uint256 amount,bytes32 metadataHash,uint256 platformFeeBps,uint256 deadline)"
    );

    /// @dev Signed by `job.buyer`, submitted alongside {assignPayee}. Binds
    ///      the exact new payee so a compromised relayer cannot redirect an
    ///      already-funded job to an attacker-controlled address.
    bytes32 private constant ASSIGN_PAYEE_TYPEHASH = keccak256(
        "AssignPayeeAuthorization(bytes32 id,address newPayee,uint256 deadline)"
    );

    /// @dev Signed by `job.buyer`, submitted alongside {releaseJob}. Binds
    ///      the exact job id so a compromised relayer cannot force-settle a
    ///      job the buyer never approved for release.
    bytes32 private constant RELEASE_JOB_TYPEHASH = keccak256(
        "ReleaseJobAuthorization(bytes32 id,uint256 deadline)"
    );

    // -- Events ------------------------------------------------------------

    event JobOpened(
        bytes32 indexed id,
        address indexed buyer,
        address indexed payee,
        address token,
        uint256 amount,
        bytes32 metadataHash,
        uint256 platformFeeBps
    );

    event JobStarted(bytes32 indexed id, uint64 startedAt, uint64 disputeDeadline);

    event JobReleased(
        bytes32 indexed id,
        address indexed payee,
        uint256 amountToPayee,
        uint256 platformFee
    );

    event JobAutoReleased(bytes32 indexed id, address indexed payee, uint256 amountToPayee);

    event JobDisputed(bytes32 indexed id, address indexed by, bytes32 evidenceHash);

    event DisputeResolved(
        bytes32 indexed id,
        address indexed operator,
        uint256 toPayee,
        uint256 toBuyer,
        bytes32 decisionHash
    );

    event JobCancelled(bytes32 indexed id, address indexed buyer, uint256 refundAmount);

    event PayeeReassigned(bytes32 indexed id, address indexed oldPayee, address indexed newPayee);

    event PlatformTreasuryUpdated(address indexed oldTreasury, address indexed newTreasury);
    event DefaultFeeUpdated(uint256 oldBps, uint256 newBps);
    event DefaultDisputeWindowUpdated(uint256 oldWindow, uint256 newWindow);

    // -- Construction ------------------------------------------------------

    /**
     * @param _platformTreasury Treasury wallet that receives the platform fee.
     *                          Should match the Mapstore Tanastok-style payout
     *                          treasury labelled on dcscan.io.
     * @param _admin            DEFAULT_ADMIN_ROLE holder. Should be the Mapstore
     *                          governance multisig.
     * @param _platform         Initial PLATFORM_ROLE holder. The Mapstore API
     *                          relayer EOA (rotated periodically).
     * @param _operator         OPERATOR_ROLE multisig.
     * @param _guardian         GUARDIAN_ROLE pauser multisig.
     */
    constructor(
        address _platformTreasury,
        address _admin,
        address _platform,
        address _operator,
        address _guardian
    ) EIP712("MapstoreEscrow", "1") {
        require(_platformTreasury != address(0), "MapstoreEscrow: treasury=0");
        require(_admin != address(0), "MapstoreEscrow: admin=0");
        require(_platform != address(0), "MapstoreEscrow: platform=0");
        require(_operator != address(0), "MapstoreEscrow: operator=0");
        require(_guardian != address(0), "MapstoreEscrow: guardian=0");

        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(PLATFORM_ROLE, _platform);
        _grantRole(OPERATOR_ROLE, _operator);
        _grantRole(GUARDIAN_ROLE, _guardian);

        platformTreasury = _platformTreasury;
        defaultPlatformFeeBps = 800;       // 8%
        defaultDisputeWindow = 7 days;
    }

    // -- Admin -------------------------------------------------------------

    function setPlatformTreasury(address newTreasury) external onlyRole(DEFAULT_ADMIN_ROLE) {
        require(newTreasury != address(0), "MapstoreEscrow: treasury=0");
        emit PlatformTreasuryUpdated(platformTreasury, newTreasury);
        platformTreasury = newTreasury;
    }

    function setDefaultPlatformFeeBps(uint256 newBps) external onlyRole(DEFAULT_ADMIN_ROLE) {
        require(newBps <= MAX_PLATFORM_FEE_BPS, "MapstoreEscrow: fee>cap");
        emit DefaultFeeUpdated(defaultPlatformFeeBps, newBps);
        defaultPlatformFeeBps = newBps;
    }

    function setDefaultDisputeWindow(uint256 newWindow) external onlyRole(DEFAULT_ADMIN_ROLE) {
        require(newWindow >= MIN_DISPUTE_WINDOW && newWindow <= MAX_DISPUTE_WINDOW,
            "MapstoreEscrow: window out of range");
        emit DefaultDisputeWindowUpdated(defaultDisputeWindow, newWindow);
        defaultDisputeWindow = newWindow;
    }

    function pause() external onlyRole(GUARDIAN_ROLE) { _pause(); }
    function unpause() external onlyRole(GUARDIAN_ROLE) { _unpause(); }

    // -- Job lifecycle -----------------------------------------------------

    /**
     * @notice Open a new escrow. The buyer must have approved this contract for
     *         at least `amount` of `token` before calling. Funds are pulled here.
     * @dev Called either directly by the buyer wallet, or by the PLATFORM_ROLE
     *      relayer on behalf of the buyer (the relayer paid the gas; the buyer's
     *      approve + signed permit handled the authorisation off-band).
     * @param id            Deterministic job id (off-chain `keccak256(jobId_str)`)
     * @param buyer         Wallet that funds the escrow. Must equal `msg.sender`
     *                      OR `msg.sender` must hold PLATFORM_ROLE.
     * @param payee         Service pro or merchant who receives funds on release.
     *                      May be `address(0)` for not-yet-matched jobs (use
     *                      `assignPayee` later, before `startJob`).
     * @param token         DCR-20 token used for settlement.
     * @param amount        Token amount (in the token's smallest units).
     * @param metadataHash  Hash of the off-chain Mapstore job descriptor.
     * @param platformFeeBps Fee in basis points. Pass `0` to use the default.
     * @param authDeadline  Unix timestamp after which `buyerAuthorization` is
     *                      no longer valid. Ignored (may be `0`) when
     *                      `msg.sender == buyer`.
     * @param buyerAuthorization EIP-712 signature from `buyer` over
     *                      {OPEN_JOB_TYPEHASH} binding this exact job.
     *                      Required whenever `msg.sender != buyer` (i.e. the
     *                      PLATFORM_ROLE relayer path). Ignored (may be
     *                      empty) when the buyer calls directly.
     */
    function openJob(
        bytes32 id,
        address buyer,
        address payee,
        IERC20 token,
        uint256 amount,
        bytes32 metadataHash,
        uint256 platformFeeBps,
        uint256 authDeadline,
        bytes calldata buyerAuthorization
    ) external nonReentrant whenNotPaused {
        require(id != bytes32(0), "MapstoreEscrow: id=0");
        require(jobs[id].status == JobStatus.None, "MapstoreEscrow: job exists");
        require(buyer != address(0), "MapstoreEscrow: buyer=0");
        require(address(token) != address(0), "MapstoreEscrow: token=0");
        require(amount > 0, "MapstoreEscrow: amount=0");

        // The relayer model: PLATFORM_ROLE may open on behalf of any buyer,
        // but ONLY with a fresh EIP-712 signature from that buyer covering
        // this exact job (id, payee, token, amount, fee) — see the
        // 2026-07-26 SECURITY note on the contract. Otherwise the buyer
        // themselves must be the caller.
        require(
            msg.sender == buyer || hasRole(PLATFORM_ROLE, msg.sender),
            "MapstoreEscrow: not buyer or platform"
        );

        if (msg.sender != buyer) {
            bytes32 structHash = keccak256(
                abi.encode(
                    OPEN_JOB_TYPEHASH,
                    id,
                    payee,
                    address(token),
                    amount,
                    metadataHash,
                    platformFeeBps,
                    authDeadline
                )
            );
            _requireBuyerAuthorization(structHash, buyer, authDeadline, buyerAuthorization);
        }

        uint256 fee = platformFeeBps == 0 ? defaultPlatformFeeBps : platformFeeBps;
        require(fee <= MAX_PLATFORM_FEE_BPS, "MapstoreEscrow: fee>cap");

        // Pull funds from the buyer. The buyer must have called
        // `token.approve(address(this), amount)` first.
        token.safeTransferFrom(buyer, address(this), amount);

        jobs[id] = Job({
            id: id,
            buyer: buyer,
            payee: payee,
            token: token,
            amount: amount,
            platformFeeBps: fee,
            metadataHash: metadataHash,
            openedAt: uint64(block.timestamp),
            startedAt: 0,
            disputeDeadline: 0,
            status: JobStatus.Pending
        });

        emit JobOpened(id, buyer, payee, address(token), amount, metadataHash, fee);
    }

    /**
     * @notice Assign or reassign the payee on a Pending job. Useful when the
     *         service pro is matched after the job is created (Pending -> Matched
     *         in the off-chain state machine).
     * @param authDeadline  Unix timestamp after which `buyerAuthorization` is
     *                      no longer valid. Ignored (may be `0`) when
     *                      `msg.sender == job.buyer`.
     * @param buyerAuthorization EIP-712 signature from `job.buyer` over
     *                      {ASSIGN_PAYEE_TYPEHASH}. Required whenever
     *                      `msg.sender != job.buyer` (PLATFORM_ROLE relayer
     *                      path) — see the 2026-07-26 SECURITY note on the
     *                      contract: this closes the "compromised relayer
     *                      redirects payout to an attacker address" vector.
     */
    function assignPayee(
        bytes32 id,
        address newPayee,
        uint256 authDeadline,
        bytes calldata buyerAuthorization
    ) external whenNotPaused {
        Job storage job = jobs[id];
        require(job.status == JobStatus.Pending, "MapstoreEscrow: not Pending");
        require(newPayee != address(0), "MapstoreEscrow: payee=0");
        require(
            hasRole(PLATFORM_ROLE, msg.sender) || msg.sender == job.buyer,
            "MapstoreEscrow: not buyer or platform"
        );

        if (msg.sender != job.buyer) {
            bytes32 structHash = keccak256(
                abi.encode(ASSIGN_PAYEE_TYPEHASH, id, newPayee, authDeadline)
            );
            _requireBuyerAuthorization(structHash, job.buyer, authDeadline, buyerAuthorization);
        }

        address old = job.payee;
        job.payee = newPayee;
        emit PayeeReassigned(id, old, newPayee);
    }

    /**
     * @notice Start the work, opening the dispute window. Pending -> InProgress.
     *         Either party may call once `payee` is set. The Mapstore relayer is
     *         the usual caller (it triggers this when the pro marks the job
     *         InProgress in the off-chain UI).
     */
    function startJob(bytes32 id) external whenNotPaused {
        Job storage job = jobs[id];
        require(job.status == JobStatus.Pending, "MapstoreEscrow: not Pending");
        require(job.payee != address(0), "MapstoreEscrow: payee unset");
        require(
            msg.sender == job.buyer || msg.sender == job.payee ||
            hasRole(PLATFORM_ROLE, msg.sender),
            "MapstoreEscrow: not party or platform"
        );

        job.status = JobStatus.InProgress;
        job.startedAt = uint64(block.timestamp);
        job.disputeDeadline = uint64(block.timestamp + defaultDisputeWindow);

        emit JobStarted(id, job.startedAt, job.disputeDeadline);
    }

    /**
     * @notice Release the escrow to the payee, minus the platform fee. Only the
     *         buyer (signalling satisfaction) or the platform relayer (acting on
     *         the buyer's `POST /jobs/{id}/complete` request) can call this
     *         before the dispute deadline.
     * @param authDeadline  Unix timestamp after which `buyerAuthorization` is
     *                      no longer valid. Ignored (may be `0`) when
     *                      `msg.sender == job.buyer`.
     * @param buyerAuthorization EIP-712 signature from `job.buyer` over
     *                      {RELEASE_JOB_TYPEHASH}. Required whenever
     *                      `msg.sender != job.buyer` (PLATFORM_ROLE relayer
     *                      path) — see the 2026-07-26 SECURITY note on the
     *                      contract: this closes the "compromised relayer
     *                      force-settles funds to its own payee" vector.
     */
    function releaseJob(
        bytes32 id,
        uint256 authDeadline,
        bytes calldata buyerAuthorization
    ) external nonReentrant whenNotPaused {
        Job storage job = jobs[id];
        require(job.status == JobStatus.InProgress, "MapstoreEscrow: not InProgress");
        require(
            msg.sender == job.buyer || hasRole(PLATFORM_ROLE, msg.sender),
            "MapstoreEscrow: not buyer or platform"
        );

        if (msg.sender != job.buyer) {
            bytes32 structHash = keccak256(abi.encode(RELEASE_JOB_TYPEHASH, id, authDeadline));
            _requireBuyerAuthorization(structHash, job.buyer, authDeadline, buyerAuthorization);
        }

        _release(job, false);
    }

    /**
     * @notice Auto-release after the dispute deadline. Permissionless: any caller
     *         can trigger this once `block.timestamp >= disputeDeadline`. This
     *         removes the platform's ability to indefinitely sit on funds.
     */
    function autoRelease(bytes32 id) external nonReentrant whenNotPaused {
        Job storage job = jobs[id];
        require(job.status == JobStatus.InProgress, "MapstoreEscrow: not InProgress");
        require(block.timestamp >= job.disputeDeadline, "MapstoreEscrow: window open");

        _release(job, true);
    }

    /**
     * @dev Verifies that `buyerAuthorization` is a valid, unexpired EIP-712
     *      signature by `buyer` over `structHash`. Reverts otherwise. Called
     *      only on the PLATFORM_ROLE relayer path (never when the buyer is
     *      `msg.sender` themselves, since a self-authored call needs no
     *      separate signature).
     */
    function _requireBuyerAuthorization(
        bytes32 structHash,
        address buyer,
        uint256 authDeadline,
        bytes calldata buyerAuthorization
    ) internal view {
        require(block.timestamp <= authDeadline, "MapstoreEscrow: authorization expired");
        bytes32 digest = _hashTypedDataV4(structHash);
        address signer = digest.recover(buyerAuthorization);
        require(signer == buyer, "MapstoreEscrow: bad buyer authorization");
    }

    function _release(Job storage job, bool isAuto) internal {
        uint256 fee = (job.amount * job.platformFeeBps) / 10_000;
        uint256 toPayee = job.amount - fee;

        job.status = JobStatus.Completed;

        if (fee > 0) {
            job.token.safeTransfer(platformTreasury, fee);
        }
        job.token.safeTransfer(job.payee, toPayee);

        if (isAuto) {
            emit JobAutoReleased(job.id, job.payee, toPayee);
        } else {
            emit JobReleased(job.id, job.payee, toPayee, fee);
        }
    }

    /**
     * @notice Open a dispute. Either party can do this once `startJob` has been
     *         called and before `releaseJob`. Funds stay locked until the
     *         operator multisig calls `resolveDispute`.
     * @param evidenceHash Hash of the off-chain dispute evidence (IPFS CID,
     *                     screenshots, message thread, etc.). The contract
     *                     stores only the hash; the bytes live off-chain so
     *                     they can be erased per GDPR Art. 17.
     */
    function openDispute(bytes32 id, bytes32 evidenceHash) external whenNotPaused {
        Job storage job = jobs[id];
        require(job.status == JobStatus.InProgress, "MapstoreEscrow: not InProgress");
        require(msg.sender == job.buyer || msg.sender == job.payee, "MapstoreEscrow: not party");

        job.status = JobStatus.Disputed;
        emit JobDisputed(id, msg.sender, evidenceHash);
    }

    /**
     * @notice Operator resolves a dispute by splitting the escrow. The two
     *         amounts must add up to exactly `job.amount` (no fee on disputes -
     *         the operator decides whether the platform takes a cut by reducing
     *         the buyer or payee share manually). Decision is logged immutably.
     */
    function resolveDispute(
        bytes32 id,
        uint256 toPayee,
        uint256 toBuyer,
        bytes32 decisionHash
    ) external nonReentrant onlyRole(OPERATOR_ROLE) {
        Job storage job = jobs[id];
        require(job.status == JobStatus.Disputed, "MapstoreEscrow: not Disputed");
        require(toPayee + toBuyer == job.amount, "MapstoreEscrow: bad split");

        job.status = JobStatus.Resolved;

        if (toPayee > 0) {
            job.token.safeTransfer(job.payee, toPayee);
        }
        if (toBuyer > 0) {
            job.token.safeTransfer(job.buyer, toBuyer);
        }

        emit DisputeResolved(id, msg.sender, toPayee, toBuyer, decisionHash);
    }

    /**
     * @notice Cancel and refund a Pending job. Only the buyer or the platform
     *         relayer can cancel; only before `startJob` has been called.
     */
    function cancelJob(bytes32 id) external nonReentrant whenNotPaused {
        Job storage job = jobs[id];
        require(job.status == JobStatus.Pending, "MapstoreEscrow: not Pending");
        require(
            msg.sender == job.buyer || hasRole(PLATFORM_ROLE, msg.sender),
            "MapstoreEscrow: not buyer or platform"
        );

        uint256 refund = job.amount;
        job.status = JobStatus.Cancelled;
        job.token.safeTransfer(job.buyer, refund);

        emit JobCancelled(id, job.buyer, refund);
    }

    // -- Views -------------------------------------------------------------

    function getJob(bytes32 id) external view returns (Job memory) {
        return jobs[id];
    }

    /// @notice Convenience computation of the Mapstore escrow_ref string the
    ///         off-chain `ServiceJob.escrow_ref` should carry once the job is
    ///         opened on chain. Format: "mapstore_escrow_v1:<network>:<id_hex>".
    function escrowRefFor(bytes32 id) external pure returns (string memory) {
        return string(abi.encodePacked("mapstore_escrow_v1:271828:", _toHex(id)));
    }

    function _toHex(bytes32 b) internal pure returns (string memory) {
        bytes memory hexChars = "0123456789abcdef";
        bytes memory out = new bytes(66);
        out[0] = "0";
        out[1] = "x";
        for (uint256 i = 0; i < 32; i++) {
            out[2 + i * 2]     = hexChars[uint8(b[i] >> 4)];
            out[2 + i * 2 + 1] = hexChars[uint8(b[i] & 0x0f)];
        }
        return string(out);
    }
}
