// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/governance/DatachainDAO.sol";
import "../src/governance/Treasury.sol";
import "../src/agents/AgentReputation.sol";
import "@openzeppelin/contracts/governance/TimelockController.sol";

/**
 * @title Deploy
 * @notice Foundry deployment script for Datachain Rope contracts
 * @dev Run with: forge script scripts/Deploy.s.sol --rpc-url mainnet --broadcast
 */
contract Deploy is Script {
    // Configuration
    uint48 constant VOTING_DELAY = 1; // 1 block
    uint32 constant VOTING_PERIOD = 45818; // ~1 week
    uint256 constant PROPOSAL_THRESHOLD = 100_000 ether;
    uint256 constant QUORUM_PERCENTAGE = 4;
    uint256 constant TIMELOCK_DELAY = 2 days;
    
    uint256 constant SPENDING_LIMIT = 100_000 ether;
    uint256 constant DAILY_LIMIT = 500_000 ether;
    
    uint256 constant MIN_STAKE = 1_000 ether;
    uint256 constant MAX_VIOLATIONS = 10;
    uint256 constant RECOVERY_RATE = 5;
    uint256 constant EPOCH_DURATION = 1 days;

    function run() external {
        // Get deployer private key from environment
        uint256 deployerPrivateKey = vm.envUint("PRIVATE_KEY");
        address deployer = vm.addr(deployerPrivateKey);
        
        // Get FAT token address (or deploy mock)
        address fatToken = vm.envOr("FAT_TOKEN_ADDRESS", address(0));
        
        console.log("==============================================");
        console.log("  DATACHAIN ROPE CONTRACT DEPLOYMENT");
        console.log("==============================================");
        console.log("Deployer:", deployer);
        
        vm.startBroadcast(deployerPrivateKey);
        
        // Deploy mock token if needed
        if (fatToken == address(0)) {
            // For testing only - in production use existing FAT token
            console.log("WARNING: Deploying mock token for testing");
            // fatToken = address(new MockERC20Votes("DC FAT", "FAT"));
        }
        
        // 1. Deploy Timelock
        address[] memory proposers = new address[](0);
        address[] memory executors = new address[](0);
        
        TimelockController timelock = new TimelockController(
            TIMELOCK_DELAY,
            proposers,
            executors,
            deployer
        );
        console.log("Timelock deployed:", address(timelock));
        
        // 2. Deploy DAO
        DatachainDAO dao = new DatachainDAO(
            IVotes(fatToken),
            timelock,
            VOTING_DELAY,
            VOTING_PERIOD,
            PROPOSAL_THRESHOLD,
            QUORUM_PERCENTAGE
        );
        console.log("DAO deployed:", address(dao));
        
        // 3. Deploy Treasury
        DatachainTreasury treasury = new DatachainTreasury(
            address(timelock),
            SPENDING_LIMIT,
            DAILY_LIMIT
        );
        console.log("Treasury deployed:", address(treasury));
        
        // 4. Deploy Agent Reputation
        AgentReputation agentRep = new AgentReputation(
            fatToken,
            MIN_STAKE,
            MAX_VIOLATIONS,
            RECOVERY_RATE,
            EPOCH_DURATION
        );
        console.log("Agent Reputation deployed:", address(agentRep));
        
        // 5. Configure Timelock roles
        bytes32 PROPOSER_ROLE = timelock.PROPOSER_ROLE();
        bytes32 EXECUTOR_ROLE = timelock.EXECUTOR_ROLE();
        bytes32 CANCELLER_ROLE = timelock.CANCELLER_ROLE();
        
        timelock.grantRole(PROPOSER_ROLE, address(dao));
        timelock.grantRole(EXECUTOR_ROLE, address(dao));
        timelock.grantRole(CANCELLER_ROLE, address(dao));
        console.log("Timelock roles configured");
        
        vm.stopBroadcast();
        
        // Summary
        console.log("==============================================");
        console.log("  DEPLOYMENT COMPLETE");
        console.log("==============================================");
        console.log("Timelock:        ", address(timelock));
        console.log("DAO:             ", address(dao));
        console.log("Treasury:        ", address(treasury));
        console.log("Agent Reputation:", address(agentRep));
    }
}
