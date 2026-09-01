// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/governance/CauseTokenFactory.sol";

/**
 * @title DeployCauseTokenFactory
 * @notice Deploys the Timelock-gated Cause Token factory on Datachain Rope (271828).
 *
 * @dev Env vars:
 *   CAUSE_TOKEN_FACTORY_OWNER   — expected DCSwapTimelock 0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c.
 *   CAUSE_TOKEN_FACTORY_GRANTOR — Foundation operator EOA authorised to call grantCause.
 */
contract DeployCauseTokenFactory is Script {
    address private constant COMPROMISED = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;

    function run() external returns (CauseTokenFactory deployed) {
        address owner_ = vm.envAddress("CAUSE_TOKEN_FACTORY_OWNER");
        address grantor_ = vm.envAddress("CAUSE_TOKEN_FACTORY_GRANTOR");

        require(msg.sender != COMPROMISED, "Refusing: deploy tx must not be signed by the compromised deployer");
        require(owner_ != COMPROMISED, "Refusing: owner must not be the compromised deployer");
        require(grantor_ != COMPROMISED, "Refusing: grantor must not be the compromised deployer");
        require(block.chainid == 271828, "Refusing to deploy on chain != 271828");

        console.log("=== CauseTokenFactory deployment ===");
        console.log("Deployer EOA:          ", msg.sender);
        console.log("Owner (Timelock):      ", owner_);
        console.log("Grantor:               ", grantor_);
        console.log("Chain ID:              ", block.chainid);

        vm.startBroadcast();
        deployed = new CauseTokenFactory(owner_, grantor_);
        vm.stopBroadcast();

        console.log("Deployed at:           ", address(deployed));
    }
}
