// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/governance/UntieRegistry.sol";

/**
 * @title DeployUntieRegistry
 * @notice Deploys the UntieRegistry on-chain audit trail for `rope_untieTx`.
 *
 * @dev Recommended pattern for the 2026-06-22 incident recovery:
 *   The rescue wallet 0xCF884C81Ed55b150CB1ABa8a69e2E9adf8F082Eb is BOTH
 *   the deployer (msg.sender) AND the initial consensusOracle. The deploy
 *   tx is signed by the rescue wallet on its air-gapped laptop and the
 *   signed hex is broadcast via the public RPC by the agent.
 *
 *   If UNTIE_REGISTRY_ORACLE is unset, the constructor argument defaults
 *   to msg.sender (the rescue wallet itself).
 *
 *   For subsequent (Phase 2+) use the oracle should be a master-node quorum
 *   aggregator address — see PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md and
 *   handover-security-audit-2026-06-11.mdc.
 *
 *   Within 72h after the recovery the operator calls rotateOracle() to
 *   transfer the role to a hardware-backed Safe multi-sig.
 *
 * @dev Hard-coded refusals:
 *   - Refuses to deploy on any chain other than 271828.
 *   - Refuses to deploy with the compromised deployer 0x60FB32ef…4195 as
 *     msg.sender (its private key is known to the attacker as of
 *     2026-06-22; using it as oracle would re-introduce the same
 *     compromise vector this contract is supposed to mitigate).
 *   - Refuses to deploy with the attacker 0xa8bd83cb…0591 as msg.sender
 *     for obvious reasons.
 */
contract DeployUntieRegistry is Script {
    address private constant COMPROMISED_DEPLOYER = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;
    address private constant ATTACKER = 0xa8bD83CBb72d12209Db2AC49D4DC3D78E7760591;

    function run() external returns (UntieRegistry deployed) {
        // Default oracle = msg.sender (the rescue wallet) unless overridden.
        address oracle;
        try vm.envAddress("UNTIE_REGISTRY_ORACLE") returns (address envOracle) {
            oracle = envOracle;
        } catch {
            oracle = msg.sender;
        }
        require(oracle != address(0), "oracle cannot be address(0)");
        require(oracle != COMPROMISED_DEPLOYER, "Refusing: oracle must not be the compromised deployer");
        require(oracle != ATTACKER, "Refusing: oracle must not be the attacker");

        require(msg.sender != COMPROMISED_DEPLOYER, "Refusing: deploy tx must not be signed by the compromised deployer");
        require(msg.sender != ATTACKER, "Refusing: deploy tx must not be signed by the attacker");

        console.log("=== UntieRegistry deployment ===");
        console.log("Deployer EOA:    ", msg.sender);
        console.log("Initial oracle:  ", oracle);
        console.log("Chain ID:        ", block.chainid);
        require(block.chainid == 271828, "Refusing to deploy on chain != 271828");

        vm.startBroadcast();
        deployed = new UntieRegistry(oracle);
        vm.stopBroadcast();

        console.log("Deployed at:     ", address(deployed));
        console.log("Tier S enabled:  ", deployed.tierEnabled(UntieRegistry.AuthorityTier.Sovereign));
        console.log("Tier F enabled:  ", deployed.tierEnabled(UntieRegistry.AuthorityTier.Federation));
        console.log("Tier U enabled:  ", deployed.tierEnabled(UntieRegistry.AuthorityTier.UserPetition));
        console.log("");
        console.log("Next step: oracle calls recordUntie(...) to declare the recovery,");
        console.log("then runs `reth state-edit` on each node, then calls confirmStateDelta(...).");
    }
}
