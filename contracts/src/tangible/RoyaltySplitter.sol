// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import {AccessControl} from "@openzeppelin/contracts/access/AccessControl.sol";

/// @title RoyaltySplitter
/// @notice EIP-2981 royalty receiver that splits a paid royalty between the
///         Datachain-network share and the per-token buyer-resale share. The
///         deed reports this contract as the single EIP-2981 receiver; the
///         DCswap router pays royalties via splitFor(tokenId) so the buyer share
///         routes to the right payee. Royalties received without a token context
///         (external marketplaces hitting receive()) go to the network treasury
///         and are reconciled off-chain.
contract RoyaltySplitter is AccessControl {
    bytes32 public constant SPLIT_ADMIN_ROLE = keccak256("SPLIT_ADMIN_ROLE");

    address public networkTreasury;
    uint96 public networkBps;
    uint96 public buyerBps;

    /// Per-token buyer-resale payee (the current holder who set the listing).
    mapping(uint256 => address) public buyerPayee;

    event BuyerPayeeSet(uint256 indexed tokenId, address indexed payee);
    event RoyaltyRouted(uint256 indexed tokenId, uint256 networkAmount, uint256 buyerAmount);
    event RoyaltyUnrouted(uint256 amount);
    event TreasuryUpdated(address indexed treasury);
    event SplitUpdated(uint96 networkBps, uint96 buyerBps);

    constructor(address admin, address treasury, uint96 networkBps_, uint96 buyerBps_) {
        require(admin != address(0) && treasury != address(0), "splitter: zero");
        require(networkBps_ + buyerBps_ > 0, "splitter: empty split");
        _grantRole(DEFAULT_ADMIN_ROLE, admin);
        _grantRole(SPLIT_ADMIN_ROLE, admin);
        networkTreasury = treasury;
        networkBps = networkBps_;
        buyerBps = buyerBps_;
    }

    function totalBps() public view returns (uint256) {
        return uint256(networkBps) + uint256(buyerBps);
    }

    function setTreasury(address treasury) external onlyRole(SPLIT_ADMIN_ROLE) {
        require(treasury != address(0), "splitter: zero");
        networkTreasury = treasury;
        emit TreasuryUpdated(treasury);
    }

    function setSplit(uint96 networkBps_, uint96 buyerBps_) external onlyRole(SPLIT_ADMIN_ROLE) {
        require(networkBps_ + buyerBps_ > 0, "splitter: empty split");
        networkBps = networkBps_;
        buyerBps = buyerBps_;
        emit SplitUpdated(networkBps_, buyerBps_);
    }

    /// @notice Bind the buyer-resale payee for a token (set by the deed at mint
    ///         and updated by the marketplace when a new holder lists).
    function setBuyerPayee(uint256 tokenId, address payee) external onlyRole(SPLIT_ADMIN_ROLE) {
        buyerPayee[tokenId] = payee;
        emit BuyerPayeeSet(tokenId, payee);
    }

    /// @notice Pay a royalty for a specific token; splits to network and buyer.
    function splitFor(uint256 tokenId) external payable {
        uint256 amount = msg.value;
        require(amount > 0, "splitter: zero value");
        uint256 total = totalBps();
        uint256 networkAmount = (amount * networkBps) / total;
        uint256 buyerAmount = amount - networkAmount;
        address buyer = buyerPayee[tokenId];
        if (buyer == address(0)) {
            // No buyer payee bound yet: everything to network treasury.
            networkAmount = amount;
            buyerAmount = 0;
        }
        _send(networkTreasury, networkAmount);
        if (buyerAmount > 0) {
            _send(buyer, buyerAmount);
        }
        emit RoyaltyRouted(tokenId, networkAmount, buyerAmount);
    }

    /// @notice Royalties paid without a token context route to the treasury.
    receive() external payable {
        _send(networkTreasury, msg.value);
        emit RoyaltyUnrouted(msg.value);
    }

    function _send(address to, uint256 amount) private {
        if (amount == 0) return;
        (bool ok, ) = payable(to).call{value: amount}("");
        require(ok, "splitter: transfer failed");
    }
}
