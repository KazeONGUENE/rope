// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "@openzeppelin/contracts/governance/Governor.sol";
import "@openzeppelin/contracts/governance/extensions/GovernorSettings.sol";
import "@openzeppelin/contracts/governance/extensions/GovernorCountingSimple.sol";
import "@openzeppelin/contracts/governance/extensions/GovernorVotes.sol";
import "@openzeppelin/contracts/governance/extensions/GovernorVotesQuorumFraction.sol";
import "@openzeppelin/contracts/governance/extensions/GovernorTimelockControl.sol";

/**
 * @title DatachainDAO
 * @notice Main governance contract for Datachain Rope network
 * @dev Implements OpenZeppelin Governor with timelock, voting, and quorum
 * 
 * Features:
 * - Proposal creation and voting
 * - Quorum requirements (4% of total supply)
 * - Timelock delay for execution (2 days)
 * - Delegation support
 * - On-chain execution
 */
contract DatachainDAO is
    Governor,
    GovernorSettings,
    GovernorCountingSimple,
    GovernorVotes,
    GovernorVotesQuorumFraction,
    GovernorTimelockControl
{
    /// @notice Minimum voting power to create a proposal
    uint256 public proposalThreshold_;

    /// @notice Event emitted when proposal threshold is updated
    event ProposalThresholdUpdated(uint256 oldThreshold, uint256 newThreshold);

    /**
     * @notice Initialize the DAO
     * @param _token The governance token (FAT)
     * @param _timelock The timelock controller
     * @param _votingDelay Delay before voting starts (in blocks)
     * @param _votingPeriod How long voting lasts (in blocks)
     * @param _proposalThreshold Minimum voting power to propose
     * @param _quorumPercentage Quorum as percentage (e.g., 4 for 4%)
     */
    constructor(
        IVotes _token,
        TimelockController _timelock,
        uint48 _votingDelay,
        uint32 _votingPeriod,
        uint256 _proposalThreshold,
        uint256 _quorumPercentage
    )
        Governor("Datachain DAO")
        GovernorSettings(_votingDelay, _votingPeriod, _proposalThreshold)
        GovernorVotes(_token)
        GovernorVotesQuorumFraction(_quorumPercentage)
        GovernorTimelockControl(_timelock)
    {
        proposalThreshold_ = _proposalThreshold;
    }

    /**
     * @notice Get the proposal threshold
     * @return The minimum voting power needed to create a proposal
     */
    function proposalThreshold()
        public
        view
        override(Governor, GovernorSettings)
        returns (uint256)
    {
        return proposalThreshold_;
    }

    /**
     * @notice Update the proposal threshold
     * @param newThreshold The new threshold
     */
    function setProposalThreshold(uint256 newThreshold) external onlyGovernance {
        uint256 oldThreshold = proposalThreshold_;
        proposalThreshold_ = newThreshold;
        emit ProposalThresholdUpdated(oldThreshold, newThreshold);
    }

    // Required overrides for multiple inheritance

    function votingDelay()
        public
        view
        override(Governor, GovernorSettings)
        returns (uint256)
    {
        return super.votingDelay();
    }

    function votingPeriod()
        public
        view
        override(Governor, GovernorSettings)
        returns (uint256)
    {
        return super.votingPeriod();
    }

    function state(uint256 proposalId)
        public
        view
        override(Governor, GovernorTimelockControl)
        returns (ProposalState)
    {
        return super.state(proposalId);
    }

    function proposalNeedsQueuing(uint256 proposalId)
        public
        view
        override(Governor, GovernorTimelockControl)
        returns (bool)
    {
        return super.proposalNeedsQueuing(proposalId);
    }

    function _queueOperations(
        uint256 proposalId,
        address[] memory targets,
        uint256[] memory values,
        bytes[] memory calldatas,
        bytes32 descriptionHash
    ) internal override(Governor, GovernorTimelockControl) returns (uint48) {
        return super._queueOperations(proposalId, targets, values, calldatas, descriptionHash);
    }

    function _executeOperations(
        uint256 proposalId,
        address[] memory targets,
        uint256[] memory values,
        bytes[] memory calldatas,
        bytes32 descriptionHash
    ) internal override(Governor, GovernorTimelockControl) {
        super._executeOperations(proposalId, targets, values, calldatas, descriptionHash);
    }

    function _cancel(
        address[] memory targets,
        uint256[] memory values,
        bytes[] memory calldatas,
        bytes32 descriptionHash
    ) internal override(Governor, GovernorTimelockControl) returns (uint256) {
        return super._cancel(targets, values, calldatas, descriptionHash);
    }

    function _executor()
        internal
        view
        override(Governor, GovernorTimelockControl)
        returns (address)
    {
        return super._executor();
    }
}
