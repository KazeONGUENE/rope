// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

/**
 * @title DatawalletClaimIssuer
 * @notice Datawallet+ acts as a native ClaimIssuer for ONCHAINID on Datachain
 *         Rope.  This contract is registered in the TrustedIssuersRegistry and
 *         enables the sovereign wallet to issue identity claims (KYC, AML,
 *         country, accredited investor, DCNFT holder, sovereign identity)
 *         without relying on any external KYC provider.
 *
 * @dev    Implements IClaimIssuer (ERC-735) so IdentityRegistry.isVerified()
 *         can call isClaimValid() during ERC-3643 transfer checks.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import "@openzeppelin/contracts/access/AccessControl.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";
import {IDatawalletClaimIssuer, IIdentity} from "./interfaces/IDatawalletClaimIssuer.sol";

contract DatawalletClaimIssuer is IDatawalletClaimIssuer, AccessControl {
    using ECDSA for bytes32;
    using MessageHashUtils for bytes32;

    // =========================================================================
    // Constants — Claim Topics
    // =========================================================================

    uint256 public constant KYC_VALIDATED       = 1;
    uint256 public constant AML_VALIDATED       = 2;
    uint256 public constant COUNTRY             = 3;
    uint256 public constant ACCREDITED_INVESTOR = 4;
    uint256 public constant DCNFT_HOLDER        = 10;
    uint256 public constant SOVEREIGN_IDENTITY  = 99;

    // =========================================================================
    // Roles
    // =========================================================================

    bytes32 public constant ISSUER_ROLE = keccak256("ISSUER_ROLE");
    bytes32 public constant REVOKER_ROLE = keccak256("REVOKER_ROLE");

    // =========================================================================
    // State
    // =========================================================================

    /// @notice Signing key used to produce EIP-191 signatures.
    address public signingKey;

    /// @notice Tracks revoked claim IDs.
    mapping(bytes32 => bool) public revokedClaims;

    /// @notice Tracks which topics are supported.
    mapping(uint256 => bool) private _supportedTopicMap;
    uint256[] private _supportedTopicsList;

    // =========================================================================
    // Constructor
    // =========================================================================

    /**
     * @param _signingKey  Address whose corresponding private key produces
     *                     claim signatures (typically a Datawallet+ backend HSM).
     * @param _admin       Admin address that can grant/revoke roles.
     */
    constructor(address _signingKey, address _admin) {
        require(_signingKey != address(0), "signing key = 0");
        require(_admin != address(0), "admin = 0");

        signingKey = _signingKey;

        _grantRole(DEFAULT_ADMIN_ROLE, _admin);
        _grantRole(ISSUER_ROLE, _admin);
        _grantRole(REVOKER_ROLE, _admin);

        _registerDefaultTopics();
    }

    // =========================================================================
    // IDatawalletClaimIssuer — Core
    // =========================================================================

    /// @inheritdoc IDatawalletClaimIssuer
    function signClaim(
        address _identity,
        uint256 _topic,
        bytes memory _data
    ) external override(IDatawalletClaimIssuer) onlyRole(ISSUER_ROLE) returns (bytes memory) {
        require(_supportedTopicMap[_topic], "unsupported topic");
        bytes32 dataHash = keccak256(abi.encode(_identity, _topic, _data));
        return abi.encodePacked(dataHash);
    }

    /// @inheritdoc IDatawalletClaimIssuer
    function issueClaimToIdentity(
        address _identity,
        uint256 _topic,
        bytes memory _data
    ) external override(IDatawalletClaimIssuer) onlyRole(ISSUER_ROLE) returns (bytes32 claimId) {
        require(_identity != address(0), "identity = 0");
        require(_supportedTopicMap[_topic], "unsupported topic");

        claimId = keccak256(abi.encode(_identity, _topic, _data, block.timestamp));

        bytes32 dataHash = keccak256(abi.encode(_identity, _topic, _data));
        bytes memory sig = abi.encodePacked(dataHash);

        IIdentity identity = IIdentity(_identity);
        identity.addClaim(_topic, 1, address(this), sig, _data, "");

        emit ClaimIssued(_identity, _topic, claimId, _data, block.timestamp);
    }

    /// @inheritdoc IDatawalletClaimIssuer
    function revokeClaim(bytes32 _claimId)
        external
        override(IDatawalletClaimIssuer)
        onlyRole(REVOKER_ROLE)
    {
        require(!revokedClaims[_claimId], "already revoked");
        revokedClaims[_claimId] = true;
        emit ClaimRevoked(_claimId, block.timestamp);
    }

    /// @inheritdoc IDatawalletClaimIssuer
    function isClaimValid(
        IIdentity _identity,
        uint256 _claimTopic,
        bytes calldata _sig,
        bytes calldata _data
    ) external view override(IDatawalletClaimIssuer) returns (bool) {
        if (!_supportedTopicMap[_claimTopic]) return false;

        bytes32 dataHash = keccak256(abi.encode(address(_identity), _claimTopic, _data));
        bytes32 claimId = keccak256(abi.encode(address(_identity), _claimTopic, _data, uint256(0)));

        if (revokedClaims[claimId]) return false;

        if (_sig.length == 32) {
            return bytes32(_sig) == dataHash;
        }

        if (_sig.length == 65) {
            bytes32 ethHash = dataHash.toEthSignedMessageHash();
            address recovered = ethHash.recover(_sig);
            return recovered == signingKey;
        }

        return false;
    }

    /// @inheritdoc IDatawalletClaimIssuer
    function supportedTopics()
        external
        view
        override(IDatawalletClaimIssuer)
        returns (uint256[] memory)
    {
        return _supportedTopicsList;
    }

    // =========================================================================
    // Admin
    // =========================================================================

    function setSigningKey(address _newKey) external onlyRole(DEFAULT_ADMIN_ROLE) {
        require(_newKey != address(0), "signing key = 0");
        signingKey = _newKey;
    }

    function addSupportedTopic(uint256 _topic) external onlyRole(DEFAULT_ADMIN_ROLE) {
        require(!_supportedTopicMap[_topic], "already supported");
        _supportedTopicMap[_topic] = true;
        _supportedTopicsList.push(_topic);
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

    // =========================================================================
    // Internal
    // =========================================================================

    function _registerDefaultTopics() private {
        uint256[6] memory defaults = [
            KYC_VALIDATED,
            AML_VALIDATED,
            COUNTRY,
            ACCREDITED_INVESTOR,
            DCNFT_HOLDER,
            SOVEREIGN_IDENTITY
        ];
        for (uint256 i = 0; i < defaults.length; i++) {
            _supportedTopicMap[defaults[i]] = true;
            _supportedTopicsList.push(defaults[i]);
        }
    }
}
