// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import {ERC721} from "@openzeppelin/contracts/token/ERC721/ERC721.sol";
import {ERC721URIStorage} from "@openzeppelin/contracts/token/ERC721/extensions/ERC721URIStorage.sol";
import {ERC2981} from "@openzeppelin/contracts/token/common/ERC2981.sol";
import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";
import {IERC721} from "@openzeppelin/contracts/token/ERC721/IERC721.sol";
import {ICertificateLifecycle} from "./ICertificateLifecycle.sol";

/// @title DCNFTDeed
/// @notice ERC-721 pre-order certificate deed for Tangible DC products on the
///         Datachain Rope. Minted LOCKED at payment: visible everywhere, but
///         non-transferable, non-listable and with dormant royalties until the
///         lifecycle contract reports the token delivered (isUnlocked). The
///         transfer guard is the authoritative lock; EIP-2981 royalties are set
///         per token at mint and become payable once unlocked.
contract DCNFTDeed is ERC721, ERC721URIStorage, ERC2981, AccessControl {
    bytes32 public constant MINTER_ROLE = keccak256("MINTER_ROLE");

    ICertificateLifecycle public immutable lifecycle;

    error TokenLocked(uint256 tokenId);

    event DeedMinted(uint256 indexed tokenId, address indexed to, string tokenURI);
    event TokenUriUpdated(uint256 indexed tokenId, string tokenURI);

    constructor(
        string memory name_,
        string memory symbol_,
        address admin,
        address lifecycle_
    ) ERC721(name_, symbol_) {
        require(admin != address(0) && lifecycle_ != address(0), "deed: zero");
        lifecycle = ICertificateLifecycle(lifecycle_);
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(MINTER_ROLE, admin);
    }

    /// @notice Mint a deed LOCKED to the buyer with its metadata URI and the
    ///         EIP-2981 royalty receiver (the splitter) and basis points.
    function mintLocked(
        address to,
        uint256 tokenId,
        string calldata uri,
        address royaltyReceiver,
        uint96 royaltyBps
    ) external onlyRole(MINTER_ROLE) {
        _safeMint(to, tokenId);
        _setTokenURI(tokenId, uri);
        if (royaltyReceiver != address(0)) {
            _setTokenRoyalty(tokenId, royaltyReceiver, royaltyBps);
        }
        emit DeedMinted(tokenId, to, uri);
    }

    /// @notice Update a token's metadata URI when the certificate is re-engraved.
    function setTokenURI(uint256 tokenId, string calldata uri) external onlyRole(MINTER_ROLE) {
        _requireOwned(tokenId);
        _setTokenURI(tokenId, uri);
        emit TokenUriUpdated(tokenId, uri);
    }

    /// @notice Update royalty on a token (e.g. when the splitter is redeployed).
    function setTokenRoyalty(uint256 tokenId, address receiver, uint96 bps)
        external
        onlyRole(MINTER_ROLE)
    {
        _setTokenRoyalty(tokenId, receiver, bps);
    }

    // --- lock enforcement -------------------------------------------------

    /// @dev Transfer guard: mint (from == 0) and burn (to == 0) are allowed;
    ///      any holder-to-holder transfer requires the token to be unlocked.
    function _update(address to, uint256 tokenId, address auth)
        internal
        override
        returns (address)
    {
        address from = _ownerOf(tokenId);
        if (from != address(0) && to != address(0) && !lifecycle.isUnlocked(tokenId)) {
            revert TokenLocked(tokenId);
        }
        return super._update(to, tokenId, auth);
    }

    /// @dev Block approvals while locked so a locked token cannot be listed.
    function approve(address to, uint256 tokenId) public override(ERC721, IERC721) {
        if (to != address(0) && !lifecycle.isUnlocked(tokenId)) {
            revert TokenLocked(tokenId);
        }
        super.approve(to, tokenId);
    }

    // --- overrides --------------------------------------------------------

    function tokenURI(uint256 tokenId)
        public
        view
        override(ERC721, ERC721URIStorage)
        returns (string memory)
    {
        return super.tokenURI(tokenId);
    }

    function supportsInterface(bytes4 interfaceId)
        public
        view
        override(ERC721, ERC721URIStorage, ERC2981, AccessControl)
        returns (bool)
    {
        return super.supportsInterface(interfaceId);
    }
}
