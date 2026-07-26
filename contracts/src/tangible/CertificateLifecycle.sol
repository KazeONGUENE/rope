// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";
import {ICertificateLifecycle} from "./ICertificateLifecycle.sol";

/// @title CertificateLifecycle
/// @notice Authoritative lock / unlock and notarisation anchor for Tangible DC
///         pre-order DCNFT certificates on the Datachain Rope (Chain ID 271828).
///         A certificate is minted in a locked state at payment; only
///         setDelivered() unlocks it, which is the single switch the deed's
///         transfer guard reads. Each material change re-anchors a keccak256
///         digest so the dcscan.io asset page reflects the latest provable state.
contract CertificateLifecycle is AccessControl, ICertificateLifecycle {
    /// Role allowed to anchor certificate digests (the operator / treasury signer).
    bytes32 public constant ANCHOR_ROLE = keccak256("ANCHOR_ROLE");
    /// Role allowed to advance state and to deliver (unlock).
    bytes32 public constant OPERATOR_ROLE = keccak256("OPERATOR_ROLE");

    mapping(uint256 => State) private _state;
    mapping(uint256 => bool) private _unlocked;
    /// Latest notarised digest per token id and per asset id (string).
    mapping(uint256 => bytes32) public digestOfToken;
    mapping(string => bytes32) public digestOfAsset;

    constructor(address admin) {
        require(admin != address(0), "lifecycle: admin zero");
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(ANCHOR_ROLE, admin);
        _grantRole(OPERATOR_ROLE, admin);
    }

    function isUnlocked(uint256 tokenId) external view returns (bool) {
        return _unlocked[tokenId];
    }

    function state(uint256 tokenId) external view returns (State) {
        return _state[tokenId];
    }

    /// @notice Advance lifecycle state (Ordered, Minted, Sold). Delivery must go
    ///         through setDelivered() so the unlock and the state move together.
    function setState(uint256 tokenId, State newState) external onlyRole(OPERATOR_ROLE) {
        require(newState != State.Unknown, "lifecycle: unknown state");
        require(newState != State.Delivered, "lifecycle: use setDelivered");
        require(!_unlocked[tokenId] || newState == State.Sold, "lifecycle: delivered is final");
        _state[tokenId] = newState;
        emit StateChanged(tokenId, newState);
    }

    /// @notice Confirm delivery and unlock the token. Idempotent: a repeated call
    ///         after unlock is a no-op so a retried webhook cannot revert the flow.
    function setDelivered(uint256 tokenId, bytes calldata proof) external onlyRole(OPERATOR_ROLE) {
        if (_unlocked[tokenId]) {
            return;
        }
        _unlocked[tokenId] = true;
        _state[tokenId] = State.Delivered;
        emit Delivered(tokenId, proof);
        emit StateChanged(tokenId, State.Delivered);
        emit Unlocked(tokenId);
    }

    /// @notice Mark a resale (post-unlock) for provenance; does not relock.
    function setSold(uint256 tokenId) external onlyRole(OPERATOR_ROLE) {
        require(_unlocked[tokenId], "lifecycle: not delivered");
        _state[tokenId] = State.Sold;
        emit StateChanged(tokenId, State.Sold);
    }

    /// @notice Anchor the keccak256 digest of the canonical certificate record.
    function anchorCertificate(
        string calldata assetId,
        uint256 tokenId,
        bytes32 digest,
        string calldata uriHint
    ) external onlyRole(ANCHOR_ROLE) {
        require(digest != bytes32(0), "lifecycle: empty digest");
        digestOfToken[tokenId] = digest;
        digestOfAsset[assetId] = digest;
        emit CertificateAnchored(assetId, tokenId, digest, uriHint, msg.sender);
    }
}
