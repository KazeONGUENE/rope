// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import "forge-std/Test.sol";
import "../../src/tangible/CertificateLifecycle.sol";
import "../../src/tangible/RoyaltySplitter.sol";
import "../../src/tangible/DCNFTDeed.sol";

contract TangibleCertificateTest is Test {
    CertificateLifecycle internal lifecycle;
    RoyaltySplitter internal splitter;
    DCNFTDeed internal deed;

    address internal alice = makeAddr("alice");
    address internal bob = makeAddr("bob");
    address internal treasury = makeAddr("treasury");
    address internal stranger = makeAddr("stranger");

    uint256 internal constant TOKEN = 1;

    function setUp() public {
        lifecycle = new CertificateLifecycle(address(this));
        splitter = new RoyaltySplitter(address(this), treasury, 300, 200);
        deed = new DCNFTDeed("Tangible DC Certificate", "TDC-CERT", address(this), address(lifecycle));
    }

    function _mint() internal {
        deed.mintLocked(alice, TOKEN, "ipfs://meta/1", address(splitter), 500);
        lifecycle.setState(TOKEN, ICertificateLifecycle.State.Minted);
    }

    function test_MintLocked_VisibleButNonTransferable() public {
        _mint();
        assertEq(deed.ownerOf(TOKEN), alice);
        assertEq(deed.tokenURI(TOKEN), "ipfs://meta/1");
        assertFalse(lifecycle.isUnlocked(TOKEN));

        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(DCNFTDeed.TokenLocked.selector, TOKEN));
        deed.transferFrom(alice, bob, TOKEN);
    }

    function test_ApproveBlockedWhileLocked() public {
        _mint();
        vm.prank(alice);
        vm.expectRevert(abi.encodeWithSelector(DCNFTDeed.TokenLocked.selector, TOKEN));
        deed.approve(bob, TOKEN);
    }

    function test_SetDelivered_UnlocksAndTransfers() public {
        _mint();
        lifecycle.setDelivered(TOKEN, bytes("carrier+craftsman proof"));
        assertTrue(lifecycle.isUnlocked(TOKEN));
        assertEq(uint256(lifecycle.state(TOKEN)), uint256(ICertificateLifecycle.State.Delivered));

        vm.prank(alice);
        deed.transferFrom(alice, bob, TOKEN);
        assertEq(deed.ownerOf(TOKEN), bob);
    }

    function test_SetDelivered_Idempotent() public {
        _mint();
        lifecycle.setDelivered(TOKEN, "");
        lifecycle.setDelivered(TOKEN, ""); // no revert, no relock
        assertTrue(lifecycle.isUnlocked(TOKEN));
    }

    function test_SetState_DeliveredRejected() public {
        _mint();
        vm.expectRevert(bytes("lifecycle: use setDelivered"));
        lifecycle.setState(TOKEN, ICertificateLifecycle.State.Delivered);
    }

    function test_RoyaltyInfo_IsEip2981() public {
        _mint();
        (address receiver, uint256 amount) = deed.royaltyInfo(TOKEN, 10_000);
        assertEq(receiver, address(splitter));
        assertEq(amount, 500); // 5.00%
    }

    function test_Splitter_RoutesNetworkAndBuyer() public {
        _mint();
        splitter.setBuyerPayee(TOKEN, alice);
        vm.deal(address(this), 1 ether);
        splitter.splitFor{value: 1000}(TOKEN);
        // network 300/500 = 600, buyer 400
        assertEq(treasury.balance, 600);
        assertEq(alice.balance, 400);
    }

    function test_Splitter_UnboundRoutesAllToTreasury() public {
        _mint();
        vm.deal(address(this), 1 ether);
        splitter.splitFor{value: 1000}(TOKEN);
        assertEq(treasury.balance, 1000);
    }

    function test_Anchor_StoresDigest() public {
        bytes32 digest = keccak256("record");
        lifecycle.anchorCertificate("TDC-AG-000001", TOKEN, digest, "https://dcscan.io/address/0x0");
        assertEq(lifecycle.digestOfToken(TOKEN), digest);
        assertEq(lifecycle.digestOfAsset("TDC-AG-000001"), digest);
    }

    function test_AccessControl_OperatorOnly() public {
        _mint();
        vm.prank(stranger);
        vm.expectRevert();
        lifecycle.setDelivered(TOKEN, "");
    }
}
