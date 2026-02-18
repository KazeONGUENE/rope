// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

/**
 * @title IDatawalletClaimIssuer
 * @notice Interface for the Datawallet+ Claim Issuer on Datachain Rope.
 *         Datawallet+ acts as a trusted ClaimIssuer in the ONCHAINID / ERC-3643
 *         ecosystem, issuing identity claims (KYC, AML, country, accreditation …)
 *         directly from the sovereign wallet without third-party KYC providers.
 *
 * @dev    Conforms to IClaimIssuer (ERC-735) and is registered in the
 *         TrustedIssuersRegistry of every T-REX token deployed on Rope.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import {IIdentity} from "@onchain-id/solidity/contracts/interface/IIdentity.sol";

interface IDatawalletClaimIssuer {
    // =========================================================================
    // Events
    // =========================================================================

    event ClaimIssued(
        address indexed identity,
        uint256 indexed topic,
        bytes32 indexed claimId,
        bytes data,
        uint256 timestamp
    );

    event ClaimRevoked(bytes32 indexed claimId, uint256 timestamp);

    // =========================================================================
    // Core Functions
    // =========================================================================

    /**
     * @notice Sign a claim for a given ONCHAINID.
     * @param _identity Address of the holder's ONCHAINID proxy.
     * @param _topic     Claim topic (1 = KYC, 2 = AML, 3 = COUNTRY …).
     * @param _data      ABI-encoded claim payload.
     * @return signature The EIP-191 signature produced by the issuer key.
     */
    function signClaim(
        address _identity,
        uint256 _topic,
        bytes memory _data
    ) external returns (bytes memory signature);

    /**
     * @notice Issue a signed claim and write it directly to the ONCHAINID.
     * @param _identity Address of the holder's ONCHAINID proxy.
     * @param _topic     Claim topic.
     * @param _data      ABI-encoded claim payload.
     * @return claimId   The keccak256 identifier of the claim.
     */
    function issueClaimToIdentity(
        address _identity,
        uint256 _topic,
        bytes memory _data
    ) external returns (bytes32 claimId);

    /**
     * @notice Revoke a previously issued claim.
     * @param _claimId The identifier returned by issueClaimToIdentity().
     */
    function revokeClaim(bytes32 _claimId) external;

    /**
     * @notice Check whether a claim is currently valid (called by
     *         IdentityRegistry during ERC-3643 transfer verification).
     * @param _identity   The ONCHAINID contract of the holder.
     * @param _claimTopic The topic to verify.
     * @param _sig        The signature attached to the claim.
     * @param _data       The data payload attached to the claim.
     * @return valid      True when the claim is authentic and not revoked.
     */
    function isClaimValid(
        IIdentity _identity,
        uint256 _claimTopic,
        bytes calldata _sig,
        bytes calldata _data
    ) external view returns (bool valid);

    /**
     * @notice Return the list of claim topics this issuer is authorised for.
     */
    function supportedTopics() external view returns (uint256[] memory);
}
