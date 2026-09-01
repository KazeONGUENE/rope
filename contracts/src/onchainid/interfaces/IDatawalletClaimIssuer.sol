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

/// @dev Minimal IIdentity interface for ONCHAINID claim operations on Rope.
interface IIdentity {
    function addClaim(
        uint256 _topic, uint256 _scheme, address _issuer,
        bytes calldata _signature, bytes calldata _data, string calldata _uri
    ) external returns (bytes32);
    function getClaim(bytes32 _claimId) external view returns (
        uint256, uint256, address, bytes memory, bytes memory, string memory
    );
    function getClaimIdsByTopic(uint256 _topic) external view returns (bytes32[] memory);
}

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
     * @notice Compute the digest that must be EIP-191-signed off-chain by
     *         the `signingKey` private-key holder before a claim can be
     *         issued via {issueClaimToIdentity}. A contract cannot itself
     *         hold or use an EOA's private key, so this function does NOT
     *         (and, as of the 2026-07-26 counter-audit fix, never again
     *         will) return a value that {isClaimValid} treats as a valid
     *         signature on its own — it only returns the pre-image the
     *         real signature is computed over.
     * @param _identity Address of the holder's ONCHAINID proxy.
     * @param _topic     Claim topic (1 = KYC, 2 = AML, 3 = COUNTRY …).
     * @param _data      ABI-encoded claim payload.
     * @return digest    keccak256(abi.encode(_identity, _topic, _data)) —
     *                    also the claim's revocation id (see
     *                    {issueClaimToIdentity} / {revokeClaim}).
     */
    function signClaim(
        address _identity,
        uint256 _topic,
        bytes memory _data
    ) external view returns (bytes memory digest);

    /**
     * @notice Issue a claim and write it directly to the ONCHAINID.
     * @dev    `_signature` MUST be a 65-byte ECDSA signature, produced
     *         off-chain by the `signingKey` private-key holder, over the
     *         EIP-191-wrapped digest returned by {signClaim}. The claim id
     *         is deterministic (`keccak256(abi.encode(_identity, _topic,
     *         _data))`, no timestamp component) so that a later
     *         {revokeClaim} call always targets the exact id this
     *         function returned — see the 2026-07-26 counter-audit fix
     *         notes in `DatawalletClaimIssuer.sol` for why the previous
     *         timestamp-keyed id made revocation a no-op against
     *         {isClaimValid}.
     * @param _identity  Address of the holder's ONCHAINID proxy.
     * @param _topic     Claim topic.
     * @param _data      ABI-encoded claim payload.
     * @param _signature 65-byte ECDSA signature over the EIP-191-wrapped
     *                    digest, recoverable to `signingKey`.
     * @return claimId   The deterministic identifier of the claim.
     */
    function issueClaimToIdentity(
        address _identity,
        uint256 _topic,
        bytes memory _data,
        bytes memory _signature
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
