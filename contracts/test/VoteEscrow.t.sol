// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../src/governance/VoteEscrow.sol";

contract VoteEscrowTest is Test {
    VoteEscrow escrow;

    address owner = address(0x50Cf); // stand-in for DCSwapTimelock
    address creator = address(0xCACA0);
    address guardian = address(0x6A2D);
    uint256 attestorPk = 0xA77E5;
    address attestor;

    address voterA = address(0xA11CE);
    address voterB = address(0xB0B);
    address voterC = address(0xC0C0);

    uint256 constant MIN_WEIGHT_TO_CREATE = 1_000_000 ether;

    function setUp() public {
        attestor = vm.addr(attestorPk);
        escrow = new VoteEscrow(owner, attestor, creator, guardian, MIN_WEIGHT_TO_CREATE);
        vm.deal(voterA, 1_000 ether);
        vm.deal(voterB, 1_000 ether);
        vm.deal(voterC, 1_000 ether);
        vm.deal(creator, 1_000 ether);
    }

    // ── helpers ─────────────────────────────────────────────────────────

    function _signCast(uint256 voteId, address voter, uint256 weight, uint256 expiresAt)
        internal
        view
        returns (bytes memory)
    {
        bytes32 digest = escrow.castWeightDigest(voteId, voter, weight, expiresAt);
        bytes32 ethSigned = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(attestorPk, ethSigned);
        return abi.encodePacked(r, s, v);
    }

    function _signCreate(address creatorAddr, uint256 weight, uint256 expiresAt)
        internal
        view
        returns (bytes memory)
    {
        bytes32 digest = escrow.createWeightDigest(creatorAddr, weight, expiresAt);
        bytes32 ethSigned = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(attestorPk, ethSigned);
        return abi.encodePacked(r, s, v);
    }

    function _defaultParams(VoteEscrow.VoteClass class_, VoteEscrow.Disposition disp)
        internal
        view
        returns (VoteEscrow.CreateVoteParams memory)
    {
        return VoteEscrow.CreateVoteParams({
            voteClass: class_,
            disposition: disp,
            startsAt: uint64(block.timestamp),
            endsAt: uint64(block.timestamp + 7 days),
            minWeightToVote: 0,
            quorumWeight: 100 ether,
            approvalThresholdBps: 5100,
            rewardPoolFunder: address(0),
            metadataHash: keccak256("proj-1"),
            eligibleVoterSet: VoteEscrow.EligibleVoterSet.AllHolders,
            payToVoteFee: 0
        });
    }

    function _juryAndPayParams(VoteEscrow.VoteClass class_, VoteEscrow.Disposition disp, uint256 payFee)
        internal
        view
        returns (VoteEscrow.CreateVoteParams memory)
    {
        VoteEscrow.CreateVoteParams memory p = _defaultParams(class_, disp);
        p.eligibleVoterSet = VoteEscrow.EligibleVoterSet.JuryAndPay;
        p.payToVoteFee = payFee;
        return p;
    }

    function _createJuryAndPay(VoteEscrow.VoteClass class_, VoteEscrow.Disposition disp, uint256 payFee)
        internal
        returns (uint256 voteId)
    {
        vm.prank(creator);
        voteId = escrow.createVote(_juryAndPayParams(class_, disp, payFee), 0, 0, "");
    }

    function _createAsCreator(VoteEscrow.VoteClass class_, VoteEscrow.Disposition disp, uint256 rewardValue)
        internal
        returns (uint256 voteId)
    {
        vm.prank(creator);
        voteId = escrow.createVote{value: rewardValue}(_defaultParams(class_, disp), 0, 0, "");
    }

    // ── constructor / deployment invariants ──────────────────────────

    function test_deploymentState() public view {
        assertEq(escrow.owner(), owner);
        assertEq(escrow.attestor(), attestor);
        assertEq(escrow.creator(), creator);
        assertEq(escrow.guardian(), guardian);
        assertEq(escrow.minWeightToCreate(), MIN_WEIGHT_TO_CREATE);
        assertFalse(escrow.paused());
        assertEq(escrow.votesLength(), 0);
    }

    function test_constructorRefusesCompromisedAddresses() public {
        address compromised = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.CompromisedAddress.selector, compromised));
        new VoteEscrow(compromised, attestor, creator, guardian, 0);

        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.CompromisedAddress.selector, compromised));
        new VoteEscrow(owner, compromised, creator, guardian, 0);

        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.CompromisedAddress.selector, compromised));
        new VoteEscrow(owner, attestor, compromised, guardian, 0);
    }

    function test_constructorRejectsZeroAddresses() public {
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.ZeroAddress.selector, "owner"));
        new VoteEscrow(address(0), attestor, creator, guardian, 0);

        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.ZeroAddress.selector, "attestor"));
        new VoteEscrow(owner, address(0), creator, guardian, 0);

        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.ZeroAddress.selector, "creator"));
        new VoteEscrow(owner, attestor, address(0), guardian, 0);
    }

    // ── createVote: admin-gated classes ─────────────────────────────

    function test_createVote_causeByCreator_succeeds() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn, 0);
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(uint8(v.voteClass), uint8(VoteEscrow.VoteClass.Cause));
        assertEq(v.creator, creator);
    }

    function test_createVote_criticalProtocolByCreator_succeeds() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.CriticalProtocol, VoteEscrow.Disposition.Return, 0);
        assertEq(escrow.votesLength(), id + 1);
    }

    function test_createVote_causeByCommunity_withValidAttestation_succeeds() public {
        // Cause is community-creatable (NGO/donation pipeline) — same
        // attested-weight gate as Project / NonCriticalFeature.
        uint256 weight = 2_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory attestation = _signCreate(voterA, weight, expiresAt);

        vm.prank(voterA);
        uint256 id = escrow.createVote(
            _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return), weight, expiresAt, attestation
        );
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(uint8(v.voteClass), uint8(VoteEscrow.VoteClass.Cause));
        assertEq(v.creator, voterA);
    }

    function test_createVote_causeByRandomWallet_withoutAttestation_reverts() public {
        vm.prank(voterA);
        vm.expectRevert(); // InvalidAttestation / AttestationExpired — not NotCreator
        escrow.createVote(_defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn), 0, 0, "");
    }

    function test_createVote_criticalProtocolByRandomWallet_reverts() public {
        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NotCreator.selector, voterA, creator));
        escrow.createVote(_defaultParams(VoteEscrow.VoteClass.CriticalProtocol, VoteEscrow.Disposition.Return), 0, 0, "");
    }

    // ── createVote: community-gated classes ─────────────────────────

    function test_createVote_projectByCommunity_withValidAttestation_succeeds() public {
        uint256 weight = 2_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory attestation = _signCreate(voterA, weight, expiresAt);

        vm.prank(voterA);
        uint256 id = escrow.createVote(
            _defaultParams(VoteEscrow.VoteClass.Project, VoteEscrow.Disposition.Return), weight, expiresAt, attestation
        );
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(v.creator, voterA);
    }

    function test_createVote_projectByCommunity_belowMinWeight_reverts() public {
        uint256 weight = 500_000 ether; // below MIN_WEIGHT_TO_CREATE
        uint256 expiresAt = block.timestamp + 300;
        bytes memory attestation = _signCreate(voterA, weight, expiresAt);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.WeightTooLow.selector, weight, MIN_WEIGHT_TO_CREATE));
        escrow.createVote(
            _defaultParams(VoteEscrow.VoteClass.Project, VoteEscrow.Disposition.Return), weight, expiresAt, attestation
        );
    }

    function test_createVote_projectByCommunity_expiredAttestation_reverts() public {
        uint256 weight = 2_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory attestation = _signCreate(voterA, weight, expiresAt);
        vm.warp(block.timestamp + 301);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.AttestationExpired.selector, expiresAt, block.timestamp));
        escrow.createVote(
            _defaultParams(VoteEscrow.VoteClass.Project, VoteEscrow.Disposition.Return), weight, expiresAt, attestation
        );
    }

    function test_createVote_projectByCommunity_wrongSigner_reverts() public {
        uint256 weight = 2_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        uint256 wrongPk = 0xBAD1;
        bytes32 digest = escrow.createWeightDigest(voterA, weight, expiresAt);
        bytes32 ethSigned = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(wrongPk, ethSigned);
        bytes memory badAttestation = abi.encodePacked(r, s, v);

        vm.prank(voterA);
        vm.expectRevert(VoteEscrow.InvalidAttestation.selector);
        escrow.createVote(
            _defaultParams(VoteEscrow.VoteClass.Project, VoteEscrow.Disposition.Return), weight, expiresAt, badAttestation
        );
    }

    function test_createVote_projectByCommunity_reusedAttestationForDifferentVoter_reverts() public {
        // Attestation for voterA cannot be presented by voterB (digest binds the signer).
        uint256 weight = 2_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory attestation = _signCreate(voterA, weight, expiresAt);

        vm.prank(voterB);
        vm.expectRevert(VoteEscrow.InvalidAttestation.selector);
        escrow.createVote(
            _defaultParams(VoteEscrow.VoteClass.Project, VoteEscrow.Disposition.Return), weight, expiresAt, attestation
        );
    }

    function test_createVote_castAttestationCannotAuthorizeCreate() public {
        // A cast-purpose digest signed for some voteId/voter must NOT satisfy
        // createVote's eligibility check even if numerically identical fields.
        uint256 weight = 2_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes32 castDigest = escrow.castWeightDigest(0, voterA, weight, expiresAt);
        bytes32 ethSigned = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", castDigest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(attestorPk, ethSigned);
        bytes memory castAttestation = abi.encodePacked(r, s, v);

        vm.prank(voterA);
        vm.expectRevert(VoteEscrow.InvalidAttestation.selector);
        escrow.createVote(
            _defaultParams(VoteEscrow.VoteClass.Project, VoteEscrow.Disposition.Return), weight, expiresAt, castAttestation
        );
    }

    // ── createVote: window / threshold / funding validation ─────────

    function test_createVote_invalidWindow_reverts() public {
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn);
        p.startsAt = uint64(block.timestamp + 100);
        p.endsAt = uint64(block.timestamp + 50); // ends before it starts
        vm.prank(creator);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.VoteWindowInvalid.selector, p.startsAt, p.endsAt));
        escrow.createVote(p, 0, 0, "");
    }

    function test_createVote_thresholdOutOfRange_reverts() public {
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn);
        p.approvalThresholdBps = 10_001;
        vm.prank(creator);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.ThresholdOutOfRange.selector, p.approvalThresholdBps));
        escrow.createVote(p, 0, 0, "");
    }

    function test_createVote_rewardWithoutFunding_reverts() public {
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward);
        vm.prank(creator);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.RewardFundingMismatch.selector, 0, VoteEscrow.Disposition.Reward));
        escrow.createVote(p, 0, 0, "");
    }

    function test_createVote_burnWithUnexpectedFunding_reverts() public {
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn);
        vm.prank(creator);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.RewardFundingMismatch.selector, 1 ether, VoteEscrow.Disposition.Burn));
        escrow.createVote{value: 1 ether}(p, 0, 0, "");
    }

    function test_createVote_whilePaused_reverts() public {
        vm.prank(owner);
        escrow.pause();
        vm.prank(creator);
        vm.expectRevert(VoteEscrow.IsPaused.selector);
        escrow.createVote(_defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn), 0, 0, "");
    }

    // ── castVote ──────────────────────────────────────────────────────

    function test_castVote_happyPath_recordsBallotAndTally() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        uint256 weight = 5_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);

        vm.prank(voterA);
        escrow.castVote{value: 10 ether}(id, true, weight, expiresAt, sig);

        VoteEscrow.Ballot memory b = escrow.getBallot(id, voterA);
        assertTrue(b.voted);
        assertTrue(b.choice);
        assertEq(b.weight, weight);
        assertEq(b.locked, 10 ether);

        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(v.totalWeightFor, weight);
        assertEq(v.totalLockedFor, 10 ether);
        assertEq(v.totalWeightAgainst, 0);
    }

    function test_castVote_zeroStakeStillCountsWeight() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        uint256 weight = 3_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);

        vm.prank(voterA);
        escrow.castVote(id, true, weight, expiresAt, sig); // no msg.value

        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(v.totalWeightFor, weight);
        assertEq(v.totalLockedFor, 0);
    }

    function test_castVote_beforeStart_reverts() public {
        vm.prank(creator);
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return);
        p.startsAt = uint64(block.timestamp + 1000);
        p.endsAt = uint64(block.timestamp + 2000);
        uint256 id = escrow.createVote(p, 0, 0, "");

        uint256 weight = 1_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);
        vm.prank(voterA);
        vm.expectRevert(
            abi.encodeWithSelector(VoteEscrow.VoteNotOpen.selector, block.timestamp, p.startsAt, p.endsAt)
        );
        escrow.castVote(id, true, weight, expiresAt, sig);
    }

    function test_castVote_afterEnd_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        vm.warp(block.timestamp + 8 days);

        uint256 weight = 1_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);
        vm.prank(voterA);
        vm.expectRevert();
        escrow.castVote(id, true, weight, expiresAt, sig);
    }

    function test_castVote_doubleVoting_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        uint256 weight = 1_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);

        vm.prank(voterA);
        escrow.castVote(id, true, weight, expiresAt, sig);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.AlreadyVoted.selector, id, voterA));
        escrow.castVote(id, false, weight, expiresAt, sig);
    }

    function test_castVote_stakeExceedsWeight_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        uint256 weight = 5 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.StakeExceedsWeight.selector, 10 ether, weight));
        escrow.castVote{value: 10 ether}(id, true, weight, expiresAt, sig);
    }

    function test_castVote_belowMinWeightToVote_reverts() public {
        vm.prank(creator);
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return);
        p.minWeightToVote = 1_000_000 ether;
        uint256 id = escrow.createVote(p, 0, 0, "");

        uint256 weight = 500 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);
        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.WeightTooLow.selector, weight, p.minWeightToVote));
        escrow.castVote(id, true, weight, expiresAt, sig);
    }

    function test_castVote_wrongSigner_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        uint256 weight = 1_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        uint256 wrongPk = 0xBAD2;
        bytes32 digest = escrow.castWeightDigest(id, voterA, weight, expiresAt);
        bytes32 ethSigned = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", digest));
        (uint8 v, bytes32 r, bytes32 s) = vm.sign(wrongPk, ethSigned);
        bytes memory badSig = abi.encodePacked(r, s, v);

        vm.prank(voterA);
        vm.expectRevert(VoteEscrow.InvalidAttestation.selector);
        escrow.castVote(id, true, weight, expiresAt, badSig);
    }

    function test_castVote_signatureBoundToVoteId_cannotReplayAcrossVotes() public {
        uint256 id0 = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        uint256 id1 = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        uint256 weight = 1_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sigForVote0 = _signCast(id0, voterA, weight, expiresAt);

        vm.prank(voterA);
        vm.expectRevert(VoteEscrow.InvalidAttestation.selector);
        escrow.castVote(id1, true, weight, expiresAt, sigForVote0);
    }

    function test_castVote_whilePaused_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        vm.prank(owner);
        escrow.pause();

        uint256 weight = 1_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterA, weight, expiresAt);
        vm.prank(voterA);
        vm.expectRevert(VoteEscrow.IsPaused.selector);
        escrow.castVote(id, true, weight, expiresAt, sig);
    }

    // ── finalizeVote ──────────────────────────────────────────────────

    function test_finalizeVote_approved() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        _vote(id, voterA, true, 600 ether);
        _vote(id, voterB, false, 400 ether);
        vm.warp(block.timestamp + 8 days);

        escrow.finalizeVote(id);
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertTrue(v.finalized);
        assertEq(uint8(v.outcome), uint8(VoteEscrow.Outcome.Approved)); // 60% >= 51%
    }

    function test_finalizeVote_rejected() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        _vote(id, voterA, true, 400 ether);
        _vote(id, voterB, false, 600 ether);
        vm.warp(block.timestamp + 8 days);

        escrow.finalizeVote(id);
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(uint8(v.outcome), uint8(VoteEscrow.Outcome.Rejected));
    }

    function test_finalizeVote_noQuorum_zeroParticipation() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        vm.warp(block.timestamp + 8 days);

        escrow.finalizeVote(id);
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(uint8(v.outcome), uint8(VoteEscrow.Outcome.NoQuorum));
    }

    function test_finalizeVote_noQuorum_belowThreshold() public {
        vm.prank(creator);
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return);
        p.quorumWeight = 1_000_000 ether; // way above what will actually vote
        uint256 id = escrow.createVote(p, 0, 0, "");
        _vote(id, voterA, true, 10 ether);
        vm.warp(block.timestamp + 8 days);

        escrow.finalizeVote(id);
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(uint8(v.outcome), uint8(VoteEscrow.Outcome.NoQuorum));
    }

    function test_finalizeVote_beforeWindowCloses_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        vm.expectRevert();
        escrow.finalizeVote(id);
    }

    function test_finalizeVote_twice_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        vm.warp(block.timestamp + 8 days);
        escrow.finalizeVote(id);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.AlreadyFinalized.selector, id));
        escrow.finalizeVote(id);
    }

    // ── Disposition: Return ──────────────────────────────────────────

    function test_return_withdrawAfterClose_refundsExactLockedAmount() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 20 ether);
        vm.warp(block.timestamp + 8 days);

        uint256 before = voterA.balance;
        vm.prank(voterA);
        escrow.withdrawLocked(id);
        assertEq(voterA.balance, before + 20 ether);

        VoteEscrow.Ballot memory b = escrow.getBallot(id, voterA);
        assertTrue(b.disposed);
    }

    function test_return_doubleWithdraw_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 20 ether);
        vm.warp(block.timestamp + 8 days);

        vm.prank(voterA);
        escrow.withdrawLocked(id);
        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.AlreadyDisposed.selector, id, voterA));
        escrow.withdrawLocked(id);
    }

    function test_return_withdrawBeforeWindowCloses_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 20 ether);

        vm.prank(voterA);
        vm.expectRevert();
        escrow.withdrawLocked(id);
    }

    function test_return_withdrawWithNoLockedFunds_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        _vote(id, voterA, true, 5_000_000 ether); // no stake
        vm.warp(block.timestamp + 8 days);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NothingLocked.selector, id, voterA));
        escrow.withdrawLocked(id);
    }

    function test_return_wrongDisposition_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn, 0);
        vm.warp(block.timestamp + 8 days);
        vm.expectRevert(
            abi.encodeWithSelector(VoteEscrow.WrongDisposition.selector, VoteEscrow.Disposition.Burn, VoteEscrow.Disposition.Return)
        );
        escrow.withdrawLocked(id);
    }

    // ── Disposition: Burn ─────────────────────────────────────────────

    function test_burn_sweepSendsEntirePoolToBurnSink() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn, 0);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 10 ether);
        _voteWithStake(id, voterB, false, 5_000_000 ether, 15 ether);
        vm.warp(block.timestamp + 8 days);

        uint256 sinkBefore = escrow.BURN_SINK().balance;
        escrow.sweepBurn(id);
        assertEq(escrow.BURN_SINK().balance, sinkBefore + 25 ether);

        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertTrue(v.burnSwept);
    }

    function test_burn_sweepTwice_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn, 0);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 10 ether);
        vm.warp(block.timestamp + 8 days);

        escrow.sweepBurn(id);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.BurnAlreadySwept.selector, id));
        escrow.sweepBurn(id);
    }

    function test_burn_sweepByAnyone_succeeds() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn, 0);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 10 ether);
        vm.warp(block.timestamp + 8 days);

        vm.prank(voterC); // not owner, not creator, not a voter on this ballot
        escrow.sweepBurn(id);
        assertEq(escrow.BURN_SINK().balance, 10 ether);
    }

    function test_burn_sweepWithZeroLocked_emitsZeroAmount() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn, 0);
        vm.warp(block.timestamp + 8 days);
        escrow.sweepBurn(id); // no revert even with nothing locked
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertTrue(v.burnSwept);
    }

    // ── Disposition: Reward ───────────────────────────────────────────

    function test_reward_proRataDistributionAfterFinalize() public {
        uint256 poolAmount = 100 ether;
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, poolAmount);

        _voteWithStake(id, voterA, true, 5_000_000 ether, 30 ether); // 30% of 100 locked
        _voteWithStake(id, voterB, false, 5_000_000 ether, 70 ether); // 70% of 100 locked
        vm.warp(block.timestamp + 8 days);
        escrow.finalizeVote(id);

        uint256 balABefore = voterA.balance;
        vm.prank(voterA);
        escrow.claimReward(id);
        // principal 30 + reward (100*30/100=30) = 60
        assertEq(voterA.balance, balABefore + 60 ether);

        uint256 balBBefore = voterB.balance;
        vm.prank(voterB);
        escrow.claimReward(id);
        // principal 70 + reward (100*70/100=70) = 140
        assertEq(voterB.balance, balBBefore + 140 ether);
    }

    function test_reward_rewardsLosingSideToo() public {
        uint256 poolAmount = 50 ether;
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, poolAmount);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 100 ether);
        _voteWithStake(id, voterB, false, 5_000_000 ether, 0); // votes against, no stake though
        _voteWithStake(id, voterC, false, 5_000_000 ether, 100 ether); // votes against WITH stake
        vm.warp(block.timestamp + 8 days);
        escrow.finalizeVote(id);

        // voterC is on the losing side but staked — must still get a reward share.
        uint256 before = voterC.balance;
        vm.prank(voterC);
        escrow.claimReward(id);
        assertGt(voterC.balance, before); // principal + pro-rata share, either way > 0 gain
    }

    function test_reward_claimBeforeFinalize_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, 10 ether);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 5 ether);
        vm.warp(block.timestamp + 8 days);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NotFinalized.selector, id));
        escrow.claimReward(id);
    }

    function test_reward_doubleClaimReverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, 10 ether);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 5 ether);
        vm.warp(block.timestamp + 8 days);
        escrow.finalizeVote(id);

        vm.prank(voterA);
        escrow.claimReward(id);
        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.AlreadyDisposed.selector, id, voterA));
        escrow.claimReward(id);
    }

    function test_reward_claimWithNoStake_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, 10 ether);
        _vote(id, voterA, true, 5_000_000 ether); // voted, no stake
        vm.warp(block.timestamp + 8 days);
        escrow.finalizeVote(id);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NothingLocked.selector, id, voterA));
        escrow.claimReward(id);
    }

    // ── reclaimUnclaimedRewardPool ────────────────────────────────────

    function test_reclaimUnclaimedRewardPool_afterGracePeriod_zeroParticipation() public {
        uint256 poolAmount = 20 ether;
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, poolAmount);
        vm.warp(block.timestamp + 7 days + 30 days + 1);

        uint256 before = creator.balance; // rewardPoolFunder defaults to creator (msg.sender at creation)
        vm.prank(owner);
        escrow.reclaimUnclaimedRewardPool(id);
        assertEq(creator.balance, before + poolAmount);
    }

    function test_reclaimUnclaimedRewardPool_beforeGracePeriod_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, 20 ether);
        vm.warp(block.timestamp + 8 days);

        vm.prank(owner);
        vm.expectRevert();
        escrow.reclaimUnclaimedRewardPool(id);
    }

    function test_reclaimUnclaimedRewardPool_withParticipation_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, 20 ether);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 5 ether);
        vm.warp(block.timestamp + 7 days + 30 days + 1);

        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NoParticipation.selector, id));
        escrow.reclaimUnclaimedRewardPool(id);
    }

    function test_reclaimUnclaimedRewardPool_notOwner_reverts() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, 20 ether);
        vm.warp(block.timestamp + 7 days + 30 days + 1);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NotOwner.selector, voterA, owner));
        escrow.reclaimUnclaimedRewardPool(id);
    }

    // ── governance / roles ────────────────────────────────────────────

    function test_pause_byOwner_succeeds() public {
        vm.prank(owner);
        escrow.pause();
        assertTrue(escrow.paused());
    }

    function test_pause_byGuardian_succeeds() public {
        vm.prank(guardian);
        escrow.pause();
        assertTrue(escrow.paused());
    }

    function test_pause_byRandomWallet_reverts() public {
        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NotOwnerOrGuardian.selector, voterA));
        escrow.pause();
    }

    function test_unpause_byGuardian_reverts() public {
        vm.prank(owner);
        escrow.pause();
        vm.prank(guardian);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NotOwner.selector, guardian, owner));
        escrow.unpause();
    }

    function test_unpause_byOwner_succeeds() public {
        vm.prank(owner);
        escrow.pause();
        vm.prank(owner);
        escrow.unpause();
        assertFalse(escrow.paused());
    }

    function test_setAttestor_refusesCompromised() public {
        address compromised = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.CompromisedAddress.selector, compromised));
        escrow.setAttestor(compromised);
    }

    function test_setAttestor_byOwner_succeeds() public {
        address newAttestor = address(0xBEEF);
        vm.prank(owner);
        escrow.setAttestor(newAttestor);
        assertEq(escrow.attestor(), newAttestor);
    }

    function test_setCreator_byNonOwner_reverts() public {
        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NotOwner.selector, voterA, owner));
        escrow.setCreator(address(0xBEEF));
    }

    function test_setMinWeightToCreate_byOwner_succeeds() public {
        vm.prank(owner);
        escrow.setMinWeightToCreate(2_000_000 ether);
        assertEq(escrow.minWeightToCreate(), 2_000_000 ether);
    }

    function test_transferOwnership_succeeds() public {
        address newOwner = address(0xF00D);
        vm.prank(owner);
        escrow.transferOwnership(newOwner);
        assertEq(escrow.owner(), newOwner);
    }

    function test_transferOwnership_refusesCompromised() public {
        address compromised = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.CompromisedAddress.selector, compromised));
        escrow.transferOwnership(compromised);
    }

    // ── escrow accounting sanity ──────────────────────────────────────

    function test_escrowBalance_reflectsLockedAndRewardFunds() public {
        uint256 id = _createAsCreator(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Reward, 10 ether);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 5 ether);
        assertEq(escrow.escrowBalance(), 15 ether); // 10 reward pool + 5 locked
    }

    // ── Phase 5: JuryAndPay eligibility ───────────────────────────────

    function test_createVote_allHolders_rejectsNonzeroPayFee() public {
        VoteEscrow.CreateVoteParams memory p = _defaultParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return);
        p.payToVoteFee = 1 ether;
        vm.prank(creator);
        vm.expectRevert(
            abi.encodeWithSelector(
                VoteEscrow.BadPayFee.selector, 1 ether, VoteEscrow.EligibleVoterSet.AllHolders
            )
        );
        escrow.createVote(p, 0, 0, "");
    }

    function test_createVote_juryAndPay_requiresPositivePayFee() public {
        VoteEscrow.CreateVoteParams memory p = _juryAndPayParams(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 0);
        vm.prank(creator);
        vm.expectRevert(
            abi.encodeWithSelector(
                VoteEscrow.BadPayFee.selector, 0, VoteEscrow.EligibleVoterSet.JuryAndPay
            )
        );
        escrow.createVote(p, 0, 0, "");
    }

    function test_payToVote_grantsRightAndAccumulatesFees() public {
        uint256 payFee = 2 ether;
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, payFee);

        vm.deal(voterA, 10 ether);
        vm.prank(voterA);
        escrow.payToVote{value: payFee}(id);

        assertTrue(escrow.hasPaidRight(id, voterA));
        assertEq(escrow.payFeeAmount(id, voterA), payFee);
        VoteEscrow.VoteConfig memory v = escrow.getVote(id);
        assertEq(v.totalPayFees, payFee);
    }

    function test_payToVote_doublePay_reverts() public {
        uint256 payFee = 1 ether;
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, payFee);

        vm.deal(voterA, 10 ether);
        vm.prank(voterA);
        escrow.payToVote{value: payFee}(id);

        vm.prank(voterA);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.AlreadyPaid.selector, id, voterA));
        escrow.payToVote{value: payFee}(id);
    }

    function test_payToVote_wrongAmount_reverts() public {
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 2 ether);

        vm.deal(voterA, 10 ether);
        vm.prank(voterA);
        vm.expectRevert(
            abi.encodeWithSelector(
                VoteEscrow.BadPayFee.selector, 1 ether, VoteEscrow.EligibleVoterSet.JuryAndPay
            )
        );
        escrow.payToVote{value: 1 ether}(id);
    }

    function test_setJury_byCreator_marksJurors() public {
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 1 ether);
        address[] memory jurors = new address[](2);
        jurors[0] = voterA;
        jurors[1] = voterB;

        vm.prank(creator);
        escrow.setJury(id, jurors);

        assertTrue(escrow.isJuror(id, voterA));
        assertTrue(escrow.isJuror(id, voterB));
        assertFalse(escrow.isJuror(id, voterC));
    }

    function test_setJury_twice_reverts() public {
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 1 ether);
        address[] memory jurors = new address[](1);
        jurors[0] = voterA;

        vm.prank(creator);
        escrow.setJury(id, jurors);

        vm.prank(creator);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.JuryAlreadySet.selector, id));
        escrow.setJury(id, jurors);
    }

    function test_castVote_juryAndPay_requiresJuryOrPaid() public {
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 1 ether);
        uint256 weight = 5_000_000 ether;
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(id, voterC, weight, expiresAt);

        vm.prank(voterC);
        vm.expectRevert(abi.encodeWithSelector(VoteEscrow.NotEligible.selector, id, voterC));
        escrow.castVote(id, true, weight, expiresAt, sig);
    }

    function test_castVote_juryAndPay_jurorCanVoteWithoutPay() public {
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, 1 ether);
        address[] memory jurors = new address[](1);
        jurors[0] = voterA;
        vm.prank(creator);
        escrow.setJury(id, jurors);

        _vote(id, voterA, true, 5_000_000 ether);
        assertTrue(escrow.hasVoted(id, voterA));
    }

    function test_castVote_juryAndPay_paidVoterCanVote() public {
        uint256 payFee = 1 ether;
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, payFee);

        vm.deal(voterB, 10 ether);
        vm.prank(voterB);
        escrow.payToVote{value: payFee}(id);

        _vote(id, voterB, true, 5_000_000 ether);
        assertTrue(escrow.hasVoted(id, voterB));
    }

    function test_withdrawPayFee_returnDisposition_refundsAfterClose() public {
        uint256 payFee = 3 ether;
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Return, payFee);

        vm.deal(voterA, 10 ether);
        vm.prank(voterA);
        escrow.payToVote{value: payFee}(id);

        vm.warp(block.timestamp + 8 days);
        uint256 before = voterA.balance;
        vm.prank(voterA);
        escrow.withdrawPayFee(id);
        assertEq(voterA.balance, before + payFee);
    }

    function test_burn_sweepIncludesPayFees() public {
        uint256 payFee = 4 ether;
        uint256 id = _createJuryAndPay(VoteEscrow.VoteClass.Cause, VoteEscrow.Disposition.Burn, payFee);

        vm.deal(voterA, 10 ether);
        vm.prank(voterA);
        escrow.payToVote{value: payFee}(id);
        _voteWithStake(id, voterA, true, 5_000_000 ether, 6 ether);

        vm.warp(block.timestamp + 8 days);
        uint256 sinkBefore = escrow.BURN_SINK().balance;
        escrow.sweepBurn(id);
        assertEq(escrow.BURN_SINK().balance, sinkBefore + 10 ether); // 6 locked + 4 pay fee
    }

    // ── internal test helpers ─────────────────────────────────────────

    function _vote(uint256 voteId, address voter, bool choice, uint256 weight) internal {
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(voteId, voter, weight, expiresAt);
        vm.prank(voter);
        escrow.castVote(voteId, choice, weight, expiresAt, sig);
    }

    function _voteWithStake(uint256 voteId, address voter, bool choice, uint256 weight, uint256 stake) internal {
        uint256 expiresAt = block.timestamp + 300;
        bytes memory sig = _signCast(voteId, voter, weight, expiresAt);
        vm.prank(voter);
        escrow.castVote{value: stake}(voteId, choice, weight, expiresAt, sig);
    }
}
