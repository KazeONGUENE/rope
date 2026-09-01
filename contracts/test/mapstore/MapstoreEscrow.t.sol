// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../../src/mapstore/MapstoreEscrow.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";
import "@openzeppelin/contracts/token/ERC20/ERC20.sol";

/// Minimal DCR-20 mock for testing. On mainnet the buyer uses the canonical
/// bridged USDC at 0xb93bd8db94f1baff474aa9cba0739daaad01641f.
contract MockDCR20 is ERC20 {
    uint8 private immutable _decimals;
    constructor(string memory name_, string memory symbol_, uint8 decimals_) ERC20(name_, symbol_) {
        _decimals = decimals_;
    }
    function decimals() public view override returns (uint8) { return _decimals; }
    function mint(address to, uint256 amount) external { _mint(to, amount); }
}

contract MapstoreEscrowTest is Test {
    MapstoreEscrow internal escrow;
    MockDCR20 internal usdc;

    address internal treasury  = makeAddr("treasury");
    address internal admin     = makeAddr("admin");
    address internal platform  = makeAddr("platform");
    address internal operator  = makeAddr("operator");
    address internal guardian  = makeAddr("guardian");
    address internal payee     = makeAddr("payee");
    address internal stranger  = makeAddr("stranger");

    // `buyer` must be a key we can sign with (EIP-712 buyer authorization
    // fix, 2026-07-26 counter-audit) — makeAddr() alone gives no private key.
    uint256 internal buyerPk = 0xB0459;
    address internal buyer   = vm.addr(buyerPk);

    uint256 internal constant FAR_FUTURE_DEADLINE = type(uint256).max;

    // -- EIP-712 signing helpers (mirror MapstoreEscrow's typehashes) ------

    bytes32 internal constant OPEN_JOB_TYPEHASH = keccak256(
        "OpenJobAuthorization(bytes32 id,address payee,address token,uint256 amount,bytes32 metadataHash,uint256 platformFeeBps,uint256 deadline)"
    );
    bytes32 internal constant ASSIGN_PAYEE_TYPEHASH = keccak256(
        "AssignPayeeAuthorization(bytes32 id,address newPayee,uint256 deadline)"
    );
    bytes32 internal constant RELEASE_JOB_TYPEHASH = keccak256(
        "ReleaseJobAuthorization(bytes32 id,uint256 deadline)"
    );

    function _domainSeparator() internal view returns (bytes32) {
        return keccak256(
            abi.encode(
                keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)"),
                keccak256(bytes("MapstoreEscrow")),
                keccak256(bytes("1")),
                block.chainid,
                address(escrow)
            )
        );
    }

    function _digest(bytes32 structHash) internal view returns (bytes32) {
        return keccak256(abi.encodePacked("\x19\x01", _domainSeparator(), structHash));
    }

    function _signOpenJob(
        uint256 pk,
        bytes32 id,
        address payeeAddr,
        address token,
        uint256 amount,
        bytes32 metadataHash,
        uint256 platformFeeBps,
        uint256 deadline
    ) internal view returns (bytes memory) {
        bytes32 structHash = keccak256(
            abi.encode(OPEN_JOB_TYPEHASH, id, payeeAddr, token, amount, metadataHash, platformFeeBps, deadline)
        );
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, _digest(structHash));
        return abi.encodePacked(r, s, v);
    }

    function _signAssignPayee(uint256 pk, bytes32 id, address newPayee, uint256 deadline)
        internal
        view
        returns (bytes memory)
    {
        bytes32 structHash = keccak256(abi.encode(ASSIGN_PAYEE_TYPEHASH, id, newPayee, deadline));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, _digest(structHash));
        return abi.encodePacked(r, s, v);
    }

    function _signRelease(uint256 pk, bytes32 id, uint256 deadline) internal view returns (bytes memory) {
        bytes32 structHash = keccak256(abi.encode(RELEASE_JOB_TYPEHASH, id, deadline));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(pk, _digest(structHash));
        return abi.encodePacked(r, s, v);
    }

    bytes32 internal constant JOB_ID = keccak256("job:0001");
    bytes32 internal constant META   = keccak256("metadata:0001");
    uint256 internal constant AMOUNT = 100_000_000; // 100 USDC (6 decimals)
    uint256 internal constant BUYER_INITIAL = 1_000 * 1e6;

    // Mirror events for assertion (must match the contract exactly).
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
    event JobReleased(bytes32 indexed id, address indexed payee, uint256 amountToPayee, uint256 platformFee);
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

    function setUp() public {
        usdc = new MockDCR20("USD Coin", "USDC", 6);
        escrow = new MapstoreEscrow(treasury, admin, platform, operator, guardian);

        usdc.mint(buyer, BUYER_INITIAL);
        vm.prank(buyer);
        usdc.approve(address(escrow), type(uint256).max);
    }

    // ------------------------------------------------------------------
    // Constructor + admin
    // ------------------------------------------------------------------

    function test_constructor_rejectsZeroAddresses() public {
        vm.expectRevert(bytes("MapstoreEscrow: treasury=0"));
        new MapstoreEscrow(address(0), admin, platform, operator, guardian);

        vm.expectRevert(bytes("MapstoreEscrow: admin=0"));
        new MapstoreEscrow(treasury, address(0), platform, operator, guardian);

        vm.expectRevert(bytes("MapstoreEscrow: platform=0"));
        new MapstoreEscrow(treasury, admin, address(0), operator, guardian);

        vm.expectRevert(bytes("MapstoreEscrow: operator=0"));
        new MapstoreEscrow(treasury, admin, platform, address(0), guardian);

        vm.expectRevert(bytes("MapstoreEscrow: guardian=0"));
        new MapstoreEscrow(treasury, admin, platform, operator, address(0));
    }

    function test_constructor_setsRolesAndDefaults() public view {
        assertTrue(escrow.hasRole(escrow.DEFAULT_ADMIN_ROLE(), admin));
        assertTrue(escrow.hasRole(escrow.PLATFORM_ROLE(), platform));
        assertTrue(escrow.hasRole(escrow.OPERATOR_ROLE(), operator));
        assertTrue(escrow.hasRole(escrow.GUARDIAN_ROLE(), guardian));

        assertEq(escrow.platformTreasury(), treasury);
        assertEq(escrow.defaultPlatformFeeBps(), 800);
        assertEq(escrow.defaultDisputeWindow(), 7 days);
    }

    function test_admin_setPlatformTreasury() public {
        address newTreasury = makeAddr("newTreasury");

        vm.prank(stranger);
        vm.expectRevert();
        escrow.setPlatformTreasury(newTreasury);

        vm.prank(admin);
        escrow.setPlatformTreasury(newTreasury);
        assertEq(escrow.platformTreasury(), newTreasury);
    }

    function test_admin_setDefaultPlatformFeeBps_capAt2000() public {
        vm.prank(admin);
        vm.expectRevert(bytes("MapstoreEscrow: fee>cap"));
        escrow.setDefaultPlatformFeeBps(2_001);

        vm.prank(admin);
        escrow.setDefaultPlatformFeeBps(1_500);
        assertEq(escrow.defaultPlatformFeeBps(), 1_500);
    }

    function test_admin_setDefaultDisputeWindow_bounds() public {
        vm.startPrank(admin);
        vm.expectRevert(bytes("MapstoreEscrow: window out of range"));
        escrow.setDefaultDisputeWindow(12 minutes);

        vm.expectRevert(bytes("MapstoreEscrow: window out of range"));
        escrow.setDefaultDisputeWindow(91 days);

        escrow.setDefaultDisputeWindow(3 days);
        vm.stopPrank();
        assertEq(escrow.defaultDisputeWindow(), 3 days);
    }

    // ------------------------------------------------------------------
    // openJob
    // ------------------------------------------------------------------

    function _openJobByBuyer() internal {
        vm.prank(buyer);
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));
    }

    function _openJobByPlatform(bytes32 id, address payeeAddr, uint256 amount, uint256 feeBps) internal {
        bytes memory sig = _signOpenJob(
            buyerPk, id, payeeAddr, address(usdc), amount, META, feeBps, FAR_FUTURE_DEADLINE
        );
        vm.prank(platform);
        escrow.openJob(id, buyer, payeeAddr, IERC20(address(usdc)), amount, META, feeBps, FAR_FUTURE_DEADLINE, sig);
    }

    function _readJob(bytes32 id)
        internal
        view
        returns (MapstoreEscrow.Job memory)
    {
        return escrow.getJob(id);
    }

    function test_openJob_pullsFundsAndEmits() public {
        uint256 escrowBalBefore = usdc.balanceOf(address(escrow));

        vm.prank(buyer);
        vm.expectEmit(true, true, true, true);
        emit JobOpened(JOB_ID, buyer, payee, address(usdc), AMOUNT, META, 800);
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));

        assertEq(usdc.balanceOf(address(escrow)), escrowBalBefore + AMOUNT);
        assertEq(usdc.balanceOf(buyer), BUYER_INITIAL - AMOUNT);

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(j.id, JOB_ID);
        assertEq(j.buyer, buyer);
        assertEq(j.payee, payee);
        assertEq(j.amount, AMOUNT);
        assertEq(j.platformFeeBps, 800);
        assertEq(j.metadataHash, META);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Pending));
    }

    function test_openJob_strangerCannotOpenForBuyer() public {
        vm.prank(stranger);
        vm.expectRevert(bytes("MapstoreEscrow: not buyer or platform"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));
    }

    function test_openJob_platformCanOpenForBuyer_withValidAuthorization() public {
        _openJobByPlatform(JOB_ID, payee, AMOUNT, 0);
        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(j.buyer, buyer);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Pending));
    }

    // 2026-07-26 counter-audit fix: PLATFORM_ROLE alone (no buyer signature)
    // must NOT be able to open a job against a victim, even though the
    // victim has an outstanding token approval to the escrow.
    function test_openJob_platformWithoutAuthorization_reverts() public {
        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: authorization expired"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));
    }

    function test_openJob_platformWithWrongSigner_reverts() public {
        uint256 attackerPk = 0xBAD1;
        bytes memory badSig = _signOpenJob(
            attackerPk, JOB_ID, payee, address(usdc), AMOUNT, META, 0, FAR_FUTURE_DEADLINE
        );
        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: bad buyer authorization"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, FAR_FUTURE_DEADLINE, badSig);
    }

    function test_openJob_platformWithExpiredAuthorization_reverts() public {
        uint256 pastDeadline = block.timestamp == 0 ? 0 : block.timestamp - 1;
        bytes memory sig = _signOpenJob(buyerPk, JOB_ID, payee, address(usdc), AMOUNT, META, 0, pastDeadline);
        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: authorization expired"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, pastDeadline, sig);
    }

    function test_openJob_platformCannotRedirectPayeeBeyondSignedAuthorization() public {
        // Buyer signs for `payee`; platform tries to substitute an attacker
        // address as the payee in the actual call. The struct hash no longer
        // matches, so recovery yields a signer != buyer and the call reverts.
        address attackerPayee = makeAddr("attackerPayee");
        bytes memory sig = _signOpenJob(buyerPk, JOB_ID, payee, address(usdc), AMOUNT, META, 0, FAR_FUTURE_DEADLINE);
        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: bad buyer authorization"));
        escrow.openJob(
            JOB_ID, buyer, attackerPayee, IERC20(address(usdc)), AMOUNT, META, 0, FAR_FUTURE_DEADLINE, sig
        );
    }

    function test_openJob_rejectsDuplicate() public {
        _openJobByBuyer();
        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: job exists"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));
    }

    function test_openJob_rejectsZeroAmount() public {
        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: amount=0"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), 0, META, 0, 0, bytes(""));
    }

    function test_openJob_rejectsFeeAboveCap() public {
        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: fee>cap"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 2_001, 0, bytes(""));
    }

    // ------------------------------------------------------------------
    // assignPayee + startJob
    // ------------------------------------------------------------------

    function test_assignPayee_pendingOnly_byBuyerOrPlatform() public {
        vm.prank(buyer);
        escrow.openJob(JOB_ID, buyer, address(0), IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));

        vm.prank(stranger);
        vm.expectRevert(bytes("MapstoreEscrow: not buyer or platform"));
        escrow.assignPayee(JOB_ID, payee, 0, bytes(""));

        vm.prank(buyer);
        vm.expectEmit(true, true, true, true);
        emit PayeeReassigned(JOB_ID, address(0), payee);
        escrow.assignPayee(JOB_ID, payee, 0, bytes(""));

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(j.payee, payee);
    }

    // 2026-07-26 counter-audit fix: PLATFORM_ROLE alone must NOT be able to
    // redirect an already-opened job's payee to an attacker address.
    function test_assignPayee_platformWithoutAuthorization_reverts() public {
        vm.prank(buyer);
        escrow.openJob(JOB_ID, buyer, address(0), IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));

        address attackerPayee = makeAddr("attackerPayee");
        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: authorization expired"));
        escrow.assignPayee(JOB_ID, attackerPayee, 0, bytes(""));
    }

    function test_assignPayee_platformWithValidAuthorization_succeeds() public {
        vm.prank(buyer);
        escrow.openJob(JOB_ID, buyer, address(0), IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));

        bytes memory sig = _signAssignPayee(buyerPk, JOB_ID, payee, FAR_FUTURE_DEADLINE);
        vm.prank(platform);
        vm.expectEmit(true, true, true, true);
        emit PayeeReassigned(JOB_ID, address(0), payee);
        escrow.assignPayee(JOB_ID, payee, FAR_FUTURE_DEADLINE, sig);

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(j.payee, payee);
    }

    function test_assignPayee_platformCannotRedirectBeyondSignedAuthorization() public {
        vm.prank(buyer);
        escrow.openJob(JOB_ID, buyer, address(0), IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));

        address attackerPayee = makeAddr("attackerPayee");
        // Buyer signed for the legitimate `payee`; platform tries to submit
        // `attackerPayee` instead using that same signature.
        bytes memory sig = _signAssignPayee(buyerPk, JOB_ID, payee, FAR_FUTURE_DEADLINE);
        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: bad buyer authorization"));
        escrow.assignPayee(JOB_ID, attackerPayee, FAR_FUTURE_DEADLINE, sig);
    }

    function test_startJob_movesToInProgress_byPayee() public {
        _openJobByBuyer();

        uint64 ts = uint64(block.timestamp);
        vm.prank(payee);
        vm.expectEmit(true, false, false, true);
        emit JobStarted(JOB_ID, ts, ts + 7 days);
        escrow.startJob(JOB_ID);

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(j.startedAt, ts);
        assertEq(j.disputeDeadline, ts + 7 days);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.InProgress));
    }

    function test_startJob_byBuyer_alsoWorks() public {
        _openJobByBuyer();
        vm.prank(buyer);
        escrow.startJob(JOB_ID);
        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.InProgress));
    }

    function test_startJob_byStranger_reverts() public {
        _openJobByBuyer();
        vm.prank(stranger);
        vm.expectRevert(bytes("MapstoreEscrow: not party or platform"));
        escrow.startJob(JOB_ID);
    }

    function test_startJob_requiresPayee() public {
        vm.prank(buyer);
        escrow.openJob(JOB_ID, buyer, address(0), IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));
        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: payee unset"));
        escrow.startJob(JOB_ID);
    }

    // ------------------------------------------------------------------
    // releaseJob (buyer release path)
    // ------------------------------------------------------------------

    function test_release_byBuyer_paysPayeeAndTreasury() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        uint256 fee = (AMOUNT * 800) / 10_000;
        uint256 net = AMOUNT - fee;

        vm.prank(buyer);
        vm.expectEmit(true, true, false, true);
        emit JobReleased(JOB_ID, payee, net, fee);
        escrow.releaseJob(JOB_ID, 0, bytes(""));

        assertEq(usdc.balanceOf(payee), net);
        assertEq(usdc.balanceOf(treasury), fee);
        assertEq(usdc.balanceOf(address(escrow)), 0);

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Completed));
    }

    function test_release_byPlatform_withValidAuthorization_works() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        bytes memory sig = _signRelease(buyerPk, JOB_ID, FAR_FUTURE_DEADLINE);
        vm.prank(platform);
        escrow.releaseJob(JOB_ID, FAR_FUTURE_DEADLINE, sig);
        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Completed));
    }

    // 2026-07-26 counter-audit fix: PLATFORM_ROLE alone (no buyer signature)
    // must NOT be able to force-settle a job to its payee.
    function test_release_byPlatformWithoutAuthorization_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: authorization expired"));
        escrow.releaseJob(JOB_ID, 0, bytes(""));
    }

    function test_release_byPlatformWithWrongSigner_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        uint256 attackerPk = 0xBAD2;
        bytes memory badSig = _signRelease(attackerPk, JOB_ID, FAR_FUTURE_DEADLINE);
        vm.prank(platform);
        vm.expectRevert(bytes("MapstoreEscrow: bad buyer authorization"));
        escrow.releaseJob(JOB_ID, FAR_FUTURE_DEADLINE, badSig);
    }

    function test_release_byStranger_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(stranger);
        vm.expectRevert(bytes("MapstoreEscrow: not buyer or platform"));
        escrow.releaseJob(JOB_ID, 0, bytes(""));
    }

    function test_release_requiresInProgress() public {
        _openJobByBuyer();
        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: not InProgress"));
        escrow.releaseJob(JOB_ID, 0, bytes(""));
    }

    function test_release_doubleRelease_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(buyer);
        escrow.releaseJob(JOB_ID, 0, bytes(""));
        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: not InProgress"));
        escrow.releaseJob(JOB_ID, 0, bytes(""));
    }

    // ------------------------------------------------------------------
    // autoRelease (anyone, after deadline)
    // ------------------------------------------------------------------

    function test_autoRelease_beforeDeadline_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(stranger);
        vm.expectRevert(bytes("MapstoreEscrow: window open"));
        escrow.autoRelease(JOB_ID);
    }

    function test_autoRelease_afterDeadline_anyCallerOK() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        vm.warp(block.timestamp + 7 days + 1);

        uint256 fee = (AMOUNT * 800) / 10_000;
        uint256 net = AMOUNT - fee;

        vm.prank(stranger);
        vm.expectEmit(true, true, false, true);
        emit JobAutoReleased(JOB_ID, payee, net);
        escrow.autoRelease(JOB_ID);

        assertEq(usdc.balanceOf(payee), net);
        assertEq(usdc.balanceOf(treasury), fee);
    }

    // ------------------------------------------------------------------
    // Dispute flow
    // ------------------------------------------------------------------

    function test_openDispute_byBuyer() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        bytes32 evidence = keccak256("evidence-001");
        vm.prank(buyer);
        vm.expectEmit(true, true, false, true);
        emit JobDisputed(JOB_ID, buyer, evidence);
        escrow.openDispute(JOB_ID, evidence);

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Disputed));
    }

    function test_openDispute_byPayee() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        vm.prank(payee);
        escrow.openDispute(JOB_ID, keccak256("evidence-002"));

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Disputed));
    }

    function test_openDispute_byStranger_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(stranger);
        vm.expectRevert(bytes("MapstoreEscrow: not party"));
        escrow.openDispute(JOB_ID, keccak256("evidence-003"));
    }

    function test_resolveDispute_byOperator_split() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(buyer);
        escrow.openDispute(JOB_ID, keccak256("evidence-004"));

        // No fee deducted on disputed resolutions per the contract spec:
        // toPayee + toBuyer == job.amount
        uint256 toPayee = (AMOUNT * 60) / 100;
        uint256 toBuyer = AMOUNT - toPayee;
        bytes32 decision = keccak256("decision-001");

        // stranger cannot resolve
        vm.prank(stranger);
        vm.expectRevert();
        escrow.resolveDispute(JOB_ID, toPayee, toBuyer, decision);

        vm.prank(operator);
        vm.expectEmit(true, true, false, true);
        emit DisputeResolved(JOB_ID, operator, toPayee, toBuyer, decision);
        escrow.resolveDispute(JOB_ID, toPayee, toBuyer, decision);

        assertEq(usdc.balanceOf(payee), toPayee);
        assertEq(usdc.balanceOf(buyer), BUYER_INITIAL - AMOUNT + toBuyer);
        // Treasury receives nothing on dispute resolution (no fee).
        assertEq(usdc.balanceOf(treasury), 0);

        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Resolved));
    }

    function test_resolveDispute_rejectsBadSplit() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(buyer);
        escrow.openDispute(JOB_ID, keccak256("evidence-005"));

        // toPayee + toBuyer != AMOUNT  ->  reject.
        vm.prank(operator);
        vm.expectRevert(bytes("MapstoreEscrow: bad split"));
        escrow.resolveDispute(JOB_ID, AMOUNT, AMOUNT, bytes32(0));
    }

    function test_resolveDispute_zeroPayee_routesAllToBuyer() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(buyer);
        escrow.openDispute(JOB_ID, keccak256("evidence-006"));

        vm.prank(operator);
        escrow.resolveDispute(JOB_ID, 0, AMOUNT, keccak256("refund-buyer"));

        assertEq(usdc.balanceOf(payee), 0);
        assertEq(usdc.balanceOf(buyer), BUYER_INITIAL); // full refund
        assertEq(usdc.balanceOf(treasury), 0);
    }

    // ------------------------------------------------------------------
    // Cancellation
    // ------------------------------------------------------------------

    function test_cancelJob_byBuyer_whilePending() public {
        _openJobByBuyer();
        uint256 escrowBalBefore = usdc.balanceOf(address(escrow));
        assertEq(escrowBalBefore, AMOUNT);

        vm.prank(buyer);
        vm.expectEmit(true, true, false, true);
        emit JobCancelled(JOB_ID, buyer, AMOUNT);
        escrow.cancelJob(JOB_ID);

        assertEq(usdc.balanceOf(buyer), BUYER_INITIAL);
        MapstoreEscrow.Job memory j = _readJob(JOB_ID);
        assertEq(uint256(j.status), uint256(MapstoreEscrow.JobStatus.Cancelled));
    }

    function test_cancelJob_byPayee_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        vm.expectRevert(bytes("MapstoreEscrow: not buyer or platform"));
        escrow.cancelJob(JOB_ID);
    }

    function test_cancelJob_inProgress_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);

        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: not Pending"));
        escrow.cancelJob(JOB_ID);
    }

    function test_cancelJob_byPlatform_whilePending() public {
        _openJobByBuyer();
        vm.prank(platform);
        escrow.cancelJob(JOB_ID);
        assertEq(usdc.balanceOf(buyer), BUYER_INITIAL);
    }

    // ------------------------------------------------------------------
    // Pausable (guardian)
    // ------------------------------------------------------------------

    function test_pause_byGuardian_blocksWrites() public {
        vm.prank(guardian);
        escrow.pause();

        vm.prank(buyer);
        vm.expectRevert();
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));

        vm.prank(guardian);
        escrow.unpause();

        vm.prank(buyer);
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));
    }

    function test_pause_byStranger_reverts() public {
        vm.prank(stranger);
        vm.expectRevert();
        escrow.pause();
    }

    // ------------------------------------------------------------------
    // Idempotency / determinism
    // ------------------------------------------------------------------

    function test_idempotency_sameJobIdAfterRelease_reverts() public {
        _openJobByBuyer();
        vm.prank(payee);
        escrow.startJob(JOB_ID);
        vm.prank(buyer);
        escrow.releaseJob(JOB_ID, 0, bytes(""));

        // Same JOB_ID after Completed MUST NOT re-open (status != None blocks).
        vm.prank(buyer);
        vm.expectRevert(bytes("MapstoreEscrow: job exists"));
        escrow.openJob(JOB_ID, buyer, payee, IERC20(address(usdc)), AMOUNT, META, 0, 0, bytes(""));
    }

    function test_escrowRefFor_format() public view {
        string memory ref = escrow.escrowRefFor(JOB_ID);
        // Format: "mapstore_escrow_v1:271828:0x<64 hex>"
        bytes memory refBytes = bytes(ref);
        assertGt(refBytes.length, 30);
        assertEq(refBytes[0], "m");
        assertEq(refBytes[1], "a");
    }

    function test_fuzz_feeMath(uint256 amount, uint256 feeBps) public {
        amount = bound(amount, 1, 10**24);
        // Pass only NON-zero feeBps since 0 means "use the default 800";
        // covering the default path is done by the non-fuzz happy-path test.
        feeBps = bound(feeBps, 1, 2_000);

        usdc.mint(buyer, amount);
        vm.prank(buyer);
        usdc.approve(address(escrow), type(uint256).max);

        bytes32 id = keccak256(abi.encode("fuzz", amount, feeBps));
        vm.prank(buyer);
        escrow.openJob(id, buyer, payee, IERC20(address(usdc)), amount, META, feeBps, 0, bytes(""));

        vm.prank(payee);
        escrow.startJob(id);

        uint256 fee = (amount * feeBps) / 10_000;
        uint256 net = amount - fee;

        uint256 treasuryBefore = usdc.balanceOf(treasury);
        uint256 payeeBefore = usdc.balanceOf(payee);

        vm.prank(buyer);
        escrow.releaseJob(id, 0, bytes(""));

        assertEq(usdc.balanceOf(treasury), treasuryBefore + fee);
        assertEq(usdc.balanceOf(payee), payeeBefore + net);
        // No tokens lost or printed: net + fee always equals amount.
        assertEq(net + fee, amount);
    }
}
