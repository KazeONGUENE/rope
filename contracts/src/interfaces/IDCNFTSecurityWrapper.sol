// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

/**
 * @title IDCNFTSecurityWrapper
 * @notice Interface binding a DCNFT (ERC-721 certificate of a tangible asset)
 *         to a security token (ERC-3643) so that the NFT represents the asset
 *         while the ERC-3643 token represents the regulated financial right.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

interface IDCNFTSecurityWrapper {
    // =========================================================================
    // Structs
    // =========================================================================

    struct AssetMetadata {
        string assetType;          // "REAL_ESTATE", "COMMODITY", "DEBT" …
        uint256 valuationUSD;      // Latest valuation in USD (oracle-fed)
        uint256 fractionCount;     // Number of fractions minted
        address custodian;         // Custodian of the physical asset
        bool physicallyRedeemable; // Whether physical redemption is allowed
        uint256 lockupUntil;       // Timestamp after which transfers unlock
    }

    // =========================================================================
    // Events
    // =========================================================================

    event SecurityTokenMinted(
        address indexed investor,
        uint256 amount,
        bytes32 claimHash
    );

    event PhysicalRedemptionRequested(
        address indexed holder,
        uint256 amount
    );

    event AssetValuationUpdated(
        uint256 newValuation,
        address indexed oracle
    );

    event CustodianUpdated(
        address indexed previousCustodian,
        address indexed newCustodian
    );

    // =========================================================================
    // View Functions
    // =========================================================================

    function dcnftContract() external view returns (address);
    function dcnftTokenId() external view returns (uint256);
    function securityToken() external view returns (address);
    function assetMetadata() external view returns (AssetMetadata memory);
    function isLocked() external view returns (bool);

    // =========================================================================
    // Mutative Functions
    // =========================================================================

    function mintSecurityTokens(
        address investor,
        uint256 amount,
        bytes32 claimHash
    ) external;

    function requestPhysicalRedemption(uint256 amount) external;

    function updateValuation(uint256 newValuationUSD) external;

    function updateCustodian(address newCustodian) external;
}
