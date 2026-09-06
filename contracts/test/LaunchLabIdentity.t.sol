// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/launchlab/LaunchLabIdentity.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

contract LaunchLabIdentityTest is Test {
    LaunchLabIdentity public implementation;
    LaunchLabIdentity public identity;
    
    address public treasury = address(0x1);
    address public user1 = address(0x2);
    address public user2 = address(0x3);
    
    uint256 public constant CREATION_FEE = 0.01 ether;
    string public constant IPFS_GATEWAY = "https://ipfs.datachain.network/ipfs/";
    string public constant VALID_CID_V0 = "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG";
    string public constant VALID_CID_V1 = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
    
    function setUp() public {
        // Deploy implementation
        implementation = new LaunchLabIdentity();
        
        // Deploy proxy
        bytes memory initData = abi.encodeWithSelector(
            LaunchLabIdentity.initialize.selector,
            treasury,
            CREATION_FEE,
            IPFS_GATEWAY
        );
        
        ERC1967Proxy proxy = new ERC1967Proxy(
            address(implementation),
            initData
        );
        
        identity = LaunchLabIdentity(address(proxy));
        
        // Fund test users
        vm.deal(user1, 10 ether);
        vm.deal(user2, 10 ether);
    }
    
    // ============ Identity Creation Tests ============
    
    function test_CreateIdentity() public {
        vm.prank(user1);
        uint256 tokenId = identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        
        assertEq(tokenId, 1);
        assertEq(identity.ownerOf(1), user1);
        assertEq(identity.walletToIdentity(user1), 1);
        assertEq(identity.hasIdentity(user1), true);
        assertEq(identity.getMetadataCID(1), VALID_CID_V0);
    }
    
    function test_CreateIdentityWithCIDv1() public {
        vm.prank(user1);
        uint256 tokenId = identity.createIdentity{value: CREATION_FEE}(VALID_CID_V1);
        
        assertEq(tokenId, 1);
        assertEq(identity.getMetadataCID(1), VALID_CID_V1);
    }
    
    function test_CreateIdentityTransfersFeeToTreasury() public {
        uint256 treasuryBalanceBefore = treasury.balance;
        
        vm.prank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        
        assertEq(treasury.balance, treasuryBalanceBefore + CREATION_FEE);
    }
    
    function test_RevertWhen_CreateIdentityTwice() public {
        vm.startPrank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        
        vm.expectRevert(
            abi.encodeWithSelector(
                LaunchLabIdentity.IdentityAlreadyExists.selector,
                user1,
                1
            )
        );
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V1);
        vm.stopPrank();
    }
    
    function test_RevertWhen_InsufficientFee() public {
        vm.prank(user1);
        vm.expectRevert(
            abi.encodeWithSelector(
                LaunchLabIdentity.InsufficientFee.selector,
                0.005 ether,
                CREATION_FEE
            )
        );
        identity.createIdentity{value: 0.005 ether}(VALID_CID_V0);
    }
    
    function test_RevertWhen_EmptyCID() public {
        vm.prank(user1);
        vm.expectRevert(LaunchLabIdentity.EmptyCID.selector);
        identity.createIdentity{value: CREATION_FEE}("");
    }
    
    function test_RevertWhen_InvalidCIDFormat() public {
        vm.prank(user1);
        vm.expectRevert(
            abi.encodeWithSelector(
                LaunchLabIdentity.InvalidCIDFormat.selector,
                "invalid_cid"
            )
        );
        identity.createIdentity{value: CREATION_FEE}("invalid_cid");
    }
    
    // ============ Metadata Update Tests ============
    
    function test_UpdateMetadata() public {
        vm.startPrank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        identity.updateMetadata(VALID_CID_V1);
        vm.stopPrank();
        
        assertEq(identity.getMetadataCID(1), VALID_CID_V1);
    }
    
    function test_RevertWhen_UpdateMetadataWithoutIdentity() public {
        vm.prank(user1);
        vm.expectRevert(
            abi.encodeWithSelector(
                LaunchLabIdentity.IdentityNotFound.selector,
                user1
            )
        );
        identity.updateMetadata(VALID_CID_V1);
    }
    
    // ============ Non-Transferability Tests ============
    
    function test_RevertWhen_Transfer() public {
        vm.prank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        
        vm.prank(user1);
        vm.expectRevert(LaunchLabIdentity.TransferNotAllowed.selector);
        identity.transferFrom(user1, user2, 1);
    }
    
    function test_RevertWhen_SafeTransfer() public {
        vm.prank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        
        vm.prank(user1);
        vm.expectRevert(LaunchLabIdentity.TransferNotAllowed.selector);
        identity.safeTransferFrom(user1, user2, 1);
    }
    
    // ============ View Function Tests ============
    
    function test_TokenURI() public {
        vm.prank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        
        string memory expectedURI = string(abi.encodePacked(IPFS_GATEWAY, VALID_CID_V0));
        assertEq(identity.tokenURI(1), expectedURI);
    }
    
    function test_GetIdentityDetails() public {
        vm.prank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        
        (
            address owner,
            string memory cid,
            uint256 createdAt,
            uint256 updatedAt
        ) = identity.getIdentityDetails(1);
        
        assertEq(owner, user1);
        assertEq(cid, VALID_CID_V0);
        assertGt(createdAt, 0);
        assertEq(createdAt, updatedAt);
    }
    
    function test_TotalIdentities() public {
        assertEq(identity.totalIdentities(), 0);
        
        vm.prank(user1);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V0);
        assertEq(identity.totalIdentities(), 1);
        
        vm.prank(user2);
        identity.createIdentity{value: CREATION_FEE}(VALID_CID_V1);
        assertEq(identity.totalIdentities(), 2);
    }
    
    // ============ Admin Function Tests ============
    
    function test_SetCreationFee() public {
        uint256 newFee = 0.02 ether;
        identity.setCreationFee(newFee);
        assertEq(identity.identityCreationFee(), newFee);
    }
    
    function test_SetTreasury() public {
        address newTreasury = address(0x4);
        identity.setTreasury(newTreasury);
        assertEq(identity.treasury(), newTreasury);
    }
    
    function test_SetIPFSGateway() public {
        string memory newGateway = "https://ipfs.io/ipfs/";
        identity.setIPFSGateway(newGateway);
        assertEq(identity.ipfsGateway(), newGateway);
    }
    
    function test_RevertWhen_NonOwnerSetsCreationFee() public {
        vm.prank(user1);
        vm.expectRevert();
        identity.setCreationFee(0.02 ether);
    }
    
    // ============ Fuzz Tests ============
    
    function testFuzz_CreateIdentityWithFee(uint256 fee) public {
        fee = bound(fee, CREATION_FEE, 100 ether);
        
        vm.deal(user1, fee);
        vm.prank(user1);
        uint256 tokenId = identity.createIdentity{value: fee}(VALID_CID_V0);
        
        assertEq(tokenId, 1);
        assertEq(treasury.balance, fee);
    }
}
