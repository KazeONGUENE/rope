// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {CauseToken} from "./CauseToken.sol";

/**
 * @title CauseTokenFactory
 * @notice Timelock-gated factory for Cause Tokens (spec §1.6 Phase 5).
 *         Foundation records a grant (owner or grantor), funds native FAT,
 *         then the NGO treasury claims once to receive the FAT grant and a
 *         deployed DCR-20 token.
 */
contract CauseTokenFactory is Ownable2Step {
    enum GrantStatus {
        Pending,
        Funded,
        Claimed
    }

    struct CauseGrant {
        bytes32 causeId;
        address ngoTreasury;
        string name;
        string symbol;
        uint256 maxSupply;
        uint256 fatGrantWei;
        GrantStatus status;
        address tokenAddress;
    }

    /// @dev Known-compromised deployer (2026-07-20 bridge audit).
    address private constant COMPROMISED = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;

    /// @notice Operational grantor (e.g. Foundation creator EOA). May call
    ///         `grantCause` without waiting on the Timelock for each NGO win.
    ///         Ownership / `setGrantor` remain Timelock-only.
    address public grantor;

    mapping(bytes32 => CauseGrant) public grants;

    error ZeroAddress(string field);
    error CompromisedAddress(address addr);
    error GrantAlreadyExists(bytes32 causeId);
    error GrantNotFound(bytes32 causeId);
    error GrantNotFunded(bytes32 causeId);
    error GrantAlreadyClaimed(bytes32 causeId);
    error FundingMismatch(uint256 sent, uint256 required);
    error NotNgoTreasury(address caller, address expected);
    error InvalidMaxSupply(uint256 maxSupply);
    error InvalidFatGrant(uint256 fatGrantWei);
    error NotOwnerOrGrantor(address caller);

    event CauseGranted(
        bytes32 indexed causeId,
        address indexed ngoTreasury,
        string name,
        string symbol,
        uint256 maxSupply,
        uint256 fatGrantWei
    );
    event CauseGrantFunded(bytes32 indexed causeId, address indexed funder, uint256 amount);
    event CauseTokenDeployed(bytes32 indexed causeId, address indexed token, address indexed ngoTreasury);
    event GrantorUpdated(address indexed oldGrantor, address indexed newGrantor);

    constructor(address owner_, address grantor_) Ownable(owner_) {
        if (owner_ == address(0)) revert ZeroAddress("owner");
        if (owner_ == COMPROMISED) revert CompromisedAddress(owner_);
        if (grantor_ == COMPROMISED) revert CompromisedAddress(grantor_);
        grantor = grantor_;
        emit GrantorUpdated(address(0), grantor_);
    }

    function setGrantor(address newGrantor) external onlyOwner {
        if (newGrantor == COMPROMISED) revert CompromisedAddress(newGrantor);
        address old = grantor;
        grantor = newGrantor;
        emit GrantorUpdated(old, newGrantor);
    }

    /// @notice Record a cause-token grant. Does not move FAT — call `fundGrant` next.
    function grantCause(
        bytes32 causeId,
        address ngoTreasury,
        string calldata name,
        string calldata symbol,
        uint256 maxSupply,
        uint256 fatGrantWei
    ) external {
        if (msg.sender != owner() && msg.sender != grantor) {
            revert NotOwnerOrGrantor(msg.sender);
        }
        if (causeId == bytes32(0)) revert GrantNotFound(causeId);
        if (ngoTreasury == address(0)) revert ZeroAddress("ngoTreasury");
        if (ngoTreasury == COMPROMISED) revert CompromisedAddress(ngoTreasury);
        if (grants[causeId].causeId != bytes32(0)) revert GrantAlreadyExists(causeId);
        if (maxSupply == 0) revert InvalidMaxSupply(maxSupply);
        if (fatGrantWei == 0) revert InvalidFatGrant(fatGrantWei);

        grants[causeId] = CauseGrant({
            causeId: causeId,
            ngoTreasury: ngoTreasury,
            name: name,
            symbol: symbol,
            maxSupply: maxSupply,
            fatGrantWei: fatGrantWei,
            status: GrantStatus.Pending,
            tokenAddress: address(0)
        });

        emit CauseGranted(causeId, ngoTreasury, name, symbol, maxSupply, fatGrantWei);
    }

    /// @notice Fund the native FAT grant for a recorded cause. Callable by anyone.
    function fundGrant(bytes32 causeId) external payable {
        CauseGrant storage g = grants[causeId];
        if (g.causeId == bytes32(0)) revert GrantNotFound(causeId);
        if (g.status != GrantStatus.Pending) revert GrantAlreadyClaimed(causeId);
        if (msg.value != g.fatGrantWei) revert FundingMismatch(msg.value, g.fatGrantWei);

        g.status = GrantStatus.Funded;
        emit CauseGrantFunded(causeId, msg.sender, msg.value);
    }

    /// @notice NGO treasury claims the FAT grant and receives a deployed CauseToken.
    function claimGrant(bytes32 causeId) external {
        CauseGrant storage g = grants[causeId];
        if (g.causeId == bytes32(0)) revert GrantNotFound(causeId);
        if (msg.sender != g.ngoTreasury) revert NotNgoTreasury(msg.sender, g.ngoTreasury);
        if (g.status == GrantStatus.Pending) revert GrantNotFunded(causeId);
        if (g.status == GrantStatus.Claimed) revert GrantAlreadyClaimed(causeId);

        g.status = GrantStatus.Claimed;

        CauseToken token = new CauseToken(g.name, g.symbol, g.maxSupply, g.ngoTreasury);
        g.tokenAddress = address(token);

        (bool ok, ) = g.ngoTreasury.call{value: g.fatGrantWei}("");
        if (!ok) revert();

        emit CauseTokenDeployed(causeId, address(token), g.ngoTreasury);
    }

    function getGrant(bytes32 causeId) external view returns (CauseGrant memory) {
        if (grants[causeId].causeId == bytes32(0)) revert GrantNotFound(causeId);
        return grants[causeId];
    }
}
