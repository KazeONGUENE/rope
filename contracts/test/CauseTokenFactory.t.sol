// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../src/governance/CauseToken.sol";
import "../src/governance/CauseTokenFactory.sol";

contract CauseTokenFactoryTest is Test {
    CauseTokenFactory factory;

    address owner = address(0x50Cf);
    address grantor = makeAddr("grantor");
    address ngoTreasury = address(0x6342);
    address funder = makeAddr("funder");

    bytes32 constant CAUSE_ID = keccak256("ngo-water-project");

    function setUp() public {
        factory = new CauseTokenFactory(owner, grantor);
        vm.deal(funder, 100 ether);
        vm.deal(ngoTreasury, 1 ether);
    }

    function test_grantCause_byGrantor_succeeds() public {
        vm.prank(grantor);
        factory.grantCause(CAUSE_ID, ngoTreasury, "Water NGO Token", "WNGO", 1_000_000 ether, 10 ether);
        CauseTokenFactory.CauseGrant memory g = factory.getGrant(CAUSE_ID);
        assertEq(uint8(g.status), uint8(CauseTokenFactory.GrantStatus.Pending));
    }

    function test_grantCause_byRandom_reverts() public {
        vm.prank(makeAddr("random"));
        vm.expectRevert(abi.encodeWithSelector(CauseTokenFactory.NotOwnerOrGrantor.selector, makeAddr("random")));
        factory.grantCause(CAUSE_ID, ngoTreasury, "Water NGO Token", "WNGO", 1_000_000 ether, 10 ether);
    }

    function test_grantCause_recordsPendingGrant() public {
        vm.prank(owner);
        factory.grantCause(CAUSE_ID, ngoTreasury, "Water NGO Token", "WNGO", 1_000_000 ether, 10 ether);

        CauseTokenFactory.CauseGrant memory g = factory.getGrant(CAUSE_ID);
        assertEq(g.ngoTreasury, ngoTreasury);
        assertEq(g.maxSupply, 1_000_000 ether);
        assertEq(g.fatGrantWei, 10 ether);
        assertEq(uint8(g.status), uint8(CauseTokenFactory.GrantStatus.Pending));
    }

    function test_fundGrant_exactAmount_marksFunded() public {
        _grantDefault();

        vm.prank(funder);
        factory.fundGrant{value: 10 ether}(CAUSE_ID);

        CauseTokenFactory.CauseGrant memory g = factory.getGrant(CAUSE_ID);
        assertEq(uint8(g.status), uint8(CauseTokenFactory.GrantStatus.Funded));
        assertEq(address(factory).balance, 10 ether);
    }

    function test_fundGrant_wrongAmount_reverts() public {
        _grantDefault();

        vm.prank(funder);
        vm.expectRevert(abi.encodeWithSelector(CauseTokenFactory.FundingMismatch.selector, 5 ether, 10 ether));
        factory.fundGrant{value: 5 ether}(CAUSE_ID);
    }

    function test_claimGrant_happyPath_deploysTokenAndSendsFat() public {
        _grantDefault();
        vm.prank(funder);
        factory.fundGrant{value: 10 ether}(CAUSE_ID);

        uint256 before = ngoTreasury.balance;
        vm.prank(ngoTreasury);
        factory.claimGrant(CAUSE_ID);

        CauseTokenFactory.CauseGrant memory g = factory.getGrant(CAUSE_ID);
        assertEq(uint8(g.status), uint8(CauseTokenFactory.GrantStatus.Claimed));
        assertTrue(g.tokenAddress != address(0));
        assertEq(ngoTreasury.balance, before + 10 ether);

        CauseToken token = CauseToken(g.tokenAddress);
        assertEq(token.name(), "Water NGO Token");
        assertEq(token.symbol(), "WNGO");
        assertEq(token.maxSupply(), 1_000_000 ether);
        assertEq(token.owner(), ngoTreasury);
        assertEq(token.minter(), ngoTreasury);
    }

    function test_claimGrant_beforeFunded_reverts() public {
        _grantDefault();

        vm.prank(ngoTreasury);
        vm.expectRevert(abi.encodeWithSelector(CauseTokenFactory.GrantNotFunded.selector, CAUSE_ID));
        factory.claimGrant(CAUSE_ID);
    }

    function test_claimGrant_notTreasury_reverts() public {
        _grantDefault();
        vm.prank(funder);
        factory.fundGrant{value: 10 ether}(CAUSE_ID);

        vm.prank(address(0xBAD));
        vm.expectRevert(
            abi.encodeWithSelector(CauseTokenFactory.NotNgoTreasury.selector, address(0xBAD), ngoTreasury)
        );
        factory.claimGrant(CAUSE_ID);
    }

    function test_grantCause_refusesCompromisedTreasury() public {
        address compromised = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;
        vm.prank(owner);
        vm.expectRevert(abi.encodeWithSelector(CauseTokenFactory.CompromisedAddress.selector, compromised));
        factory.grantCause(CAUSE_ID, compromised, "X", "X", 1 ether, 1 ether);
    }

    function test_causeToken_mintRespectsMaxSupply() public {
        _grantAndClaim();

        CauseTokenFactory.CauseGrant memory g = factory.getGrant(CAUSE_ID);
        CauseToken token = CauseToken(g.tokenAddress);

        vm.prank(ngoTreasury);
        token.mint(ngoTreasury, 100 ether);
        assertEq(token.totalSupply(), 100 ether);

        vm.prank(ngoTreasury);
        vm.expectRevert(
            abi.encodeWithSelector(CauseToken.MaxSupplyExceeded.selector, 1_000_100 ether, 1_000_000 ether)
        );
        token.mint(ngoTreasury, 1_000_000 ether);
    }

    function _grantDefault() internal {
        vm.prank(owner);
        factory.grantCause(CAUSE_ID, ngoTreasury, "Water NGO Token", "WNGO", 1_000_000 ether, 10 ether);
    }

    function _grantAndClaim() internal {
        _grantDefault();
        vm.prank(funder);
        factory.fundGrant{value: 10 ether}(CAUSE_ID);
        vm.prank(ngoTreasury);
        factory.claimGrant(CAUSE_ID);
    }
}
