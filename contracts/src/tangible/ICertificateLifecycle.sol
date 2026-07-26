// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

// Lifecycle and anchor authority for Tangible DC pre-order certificates.
// The deed contract queries isUnlocked(tokenId) in its transfer guard; only
// setDelivered() flips a token to unlocked.
interface ICertificateLifecycle {
    enum State {
        Unknown,
        Ordered,
        Minted,
        Delivered,
        Sold
    }

    event CertificateAnchored(
        string indexed assetId,
        uint256 indexed tokenId,
        bytes32 digest,
        string uriHint,
        address indexed by
    );
    event StateChanged(uint256 indexed tokenId, State state);
    event Delivered(uint256 indexed tokenId, bytes proof);
    event Unlocked(uint256 indexed tokenId);

    function isUnlocked(uint256 tokenId) external view returns (bool);

    function state(uint256 tokenId) external view returns (State);

    function setState(uint256 tokenId, State newState) external;

    function setDelivered(uint256 tokenId, bytes calldata proof) external;

    function anchorCertificate(
        string calldata assetId,
        uint256 tokenId,
        bytes32 digest,
        string calldata uriHint
    ) external;
}
