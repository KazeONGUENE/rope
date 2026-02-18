// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

/**
 * @title DCNFTSecurityWrapper
 * @notice Bridges a DCNFT (ERC-721 certificate of a tangible asset) with an
 *         ERC-3643 security token so that the NFT represents the physical asset
 *         while the security token represents the regulated financial right.
 *
 *         Only addresses whose ONCHAINID holds valid claims in the T-REX
 *         IdentityRegistry can mint / receive fractions.  An oracle can update
 *         the USD valuation, and holders may request physical redemption when
 *         the wrapper is configured to allow it.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import "@openzeppelin/contracts/access/AccessControl.sol";
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";
import "@openzeppelin/contracts/token/ERC721/IERC721.sol";
import "../interfaces/IDCNFTSecurityWrapper.sol";

interface IERC3643 {
    function mint(address _to, uint256 _amount) external;
    function balanceOf(address _owner) external view returns (uint256);
    function identityRegistry() external view returns (address);
}

contract DCNFTSecurityWrapper is IDCNFTSecurityWrapper, AccessControl, ReentrancyGuard {
    // =========================================================================
    // Roles
    // =========================================================================

    bytes32 public constant ISSUER_ROLE = keccak256("ISSUER_ROLE");
    bytes32 public constant ORACLE_ROLE = keccak256("ORACLE_ROLE");
    bytes32 public constant CUSTODIAN_ROLE = keccak256("CUSTODIAN_ROLE");

    // =========================================================================
    // Immutable References
    // =========================================================================

    IERC721 public immutable _dcnftContract;
    uint256 public immutable _dcnftTokenId;
    IERC3643 public immutable _securityToken;

    // =========================================================================
    // State
    // =========================================================================

    AssetMetadata private _metadata;

    // =========================================================================
    // Constructor
    // =========================================================================

    constructor(
        address dcnft_,
        uint256 tokenId_,
        address securityToken_,
        AssetMetadata memory metadata_,
        address admin_
    ) {
        require(dcnft_ != address(0), "dcnft = 0");
        require(securityToken_ != address(0), "security token = 0");
        require(admin_ != address(0), "admin = 0");

        _dcnftContract = IERC721(dcnft_);
        _dcnftTokenId = tokenId_;
        _securityToken = IERC3643(securityToken_);
        _metadata = metadata_;

        _grantRole(DEFAULT_ADMIN_ROLE, admin_);
        _grantRole(ISSUER_ROLE, admin_);
        _grantRole(ORACLE_ROLE, admin_);
        _grantRole(CUSTODIAN_ROLE, metadata_.custodian);
    }

    // =========================================================================
    // IDCNFTSecurityWrapper — Views
    // =========================================================================

    function dcnftContract() external view override returns (address) {
        return address(_dcnftContract);
    }

    function dcnftTokenId() external view override returns (uint256) {
        return _dcnftTokenId;
    }

    function securityToken() external view override returns (address) {
        return address(_securityToken);
    }

    function assetMetadata() external view override returns (AssetMetadata memory) {
        return _metadata;
    }

    function isLocked() external view override returns (bool) {
        return block.timestamp < _metadata.lockupUntil;
    }

    // =========================================================================
    // IDCNFTSecurityWrapper — Mutative
    // =========================================================================

    function mintSecurityTokens(
        address investor,
        uint256 amount,
        bytes32 claimHash
    ) external override onlyRole(ISSUER_ROLE) nonReentrant {
        require(investor != address(0), "investor = 0");
        require(amount > 0, "amount = 0");

        _securityToken.mint(investor, amount);
        _metadata.fractionCount += amount;

        emit SecurityTokenMinted(investor, amount, claimHash);
    }

    function requestPhysicalRedemption(uint256 amount) external override nonReentrant {
        require(_metadata.physicallyRedeemable, "redemption not allowed");
        require(block.timestamp >= _metadata.lockupUntil, "lockup active");
        require(
            _securityToken.balanceOf(msg.sender) >= amount,
            "insufficient balance"
        );

        emit PhysicalRedemptionRequested(msg.sender, amount);
    }

    function updateValuation(uint256 newValuationUSD) external override onlyRole(ORACLE_ROLE) {
        require(newValuationUSD > 0, "valuation = 0");
        _metadata.valuationUSD = newValuationUSD;
        emit AssetValuationUpdated(newValuationUSD, msg.sender);
    }

    function updateCustodian(address newCustodian) external override onlyRole(DEFAULT_ADMIN_ROLE) {
        require(newCustodian != address(0), "custodian = 0");
        address previous = _metadata.custodian;
        _metadata.custodian = newCustodian;

        _revokeRole(CUSTODIAN_ROLE, previous);
        _grantRole(CUSTODIAN_ROLE, newCustodian);

        emit CustodianUpdated(previous, newCustodian);
    }

    // =========================================================================
    // ERC-165
    // =========================================================================

    function supportsInterface(bytes4 interfaceId)
        public
        view
        override(AccessControl)
        returns (bool)
    {
        return super.supportsInterface(interfaceId);
    }
}
