// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "../src/governance/VoteEscrow.sol";

/**
 * @title DeployVoteEscrow
 * @notice Deploys the governance-voting escrow (`docs/GOVERNANCE_VOTING_CAUSE_PLATFORM_SPEC_V1.md`
 *         Phase 2) on Datachain Rope (chain 271828).
 *
 * @dev Env vars:
 *   VOTE_ESCROW_OWNER        — expected DCSwapTimelock 0x50Cfc56D81603A61660B8c6306e7Cb6E6693532c.
 *                              Defaults to msg.sender if unset (NOT recommended for production —
 *                              transfer to the Timelock immediately after deploy if you take this path).
 *   VOTE_ESCROW_ATTESTOR     — rope-explorer's cross-chain balance-aggregator signer.
 *                              REQUIRED — no default (must never silently equal the deployer).
 *   VOTE_ESCROW_CREATOR      — Foundation-operated EOA for Cause/CriticalProtocol vote creation.
 *                              REQUIRED — no default.
 *   VOTE_ESCROW_GUARDIAN     — pause-only key. Defaults to msg.sender if unset.
 *   VOTE_ESCROW_MIN_WEIGHT_TO_CREATE — wei-denominated FAT-equivalent floor for community-created
 *                              Project/NonCriticalFeature votes. Defaults to 1,000,000 * 1e18 (an
 *                              explicit, tunable starting point per spec §7 — NOT a hardcoded
 *                              business decision baked into the contract; owner can change it later
 *                              via setMinWeightToCreate()).
 *
 * @dev Hard-coded refusals (2026-07-20 bridge-audit F4/F5/F6 lesson, reused verbatim):
 *   - Refuses to deploy on any chain other than 271828.
 *   - Refuses deployer / owner / attestor / creator / guardian == the known-compromised
 *     0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195.
 *   - Refuses attestor == creator (distinct-key discipline — an attestation-signing compromise
 *     must never also compromise the admin-vote-creation path, and vice versa).
 */
contract DeployVoteEscrow is Script {
    address private constant COMPROMISED = 0x60FB32ef3A2381c2Ed71613F34fd56D56fCF4195;

    function run() external returns (VoteEscrow deployed) {
        address owner_;
        try vm.envAddress("VOTE_ESCROW_OWNER") returns (address envOwner) {
            owner_ = envOwner;
        } catch {
            owner_ = msg.sender;
        }

        address attestor = vm.envAddress("VOTE_ESCROW_ATTESTOR");
        address creator = vm.envAddress("VOTE_ESCROW_CREATOR");

        address guardian;
        try vm.envAddress("VOTE_ESCROW_GUARDIAN") returns (address envGuardian) {
            guardian = envGuardian;
        } catch {
            guardian = msg.sender;
        }

        uint256 minWeightToCreate;
        try vm.envUint("VOTE_ESCROW_MIN_WEIGHT_TO_CREATE") returns (uint256 envMin) {
            minWeightToCreate = envMin;
        } catch {
            minWeightToCreate = 1_000_000 ether; // 1,000,000 FAT-equivalent — tunable post-deploy.
        }

        require(msg.sender != COMPROMISED, "Refusing: deploy tx must not be signed by the compromised deployer");
        require(owner_ != COMPROMISED, "Refusing: owner must not be the compromised deployer");
        require(attestor != COMPROMISED, "Refusing: attestor must not be the compromised deployer");
        require(creator != COMPROMISED, "Refusing: creator must not be the compromised deployer");
        require(guardian != COMPROMISED, "Refusing: guardian must not be the compromised deployer");
        require(attestor != address(0), "attestor cannot be address(0)");
        require(creator != address(0), "creator cannot be address(0)");
        require(attestor != creator, "Refusing: attestor and creator must be distinct keys (2026-07-20 audit F6)");

        console.log("=== VoteEscrow deployment ===");
        console.log("Deployer EOA:          ", msg.sender);
        console.log("Owner (Timelock):      ", owner_);
        console.log("Attestor:              ", attestor);
        console.log("Creator:               ", creator);
        console.log("Guardian:              ", guardian);
        console.log("minWeightToCreate:     ", minWeightToCreate);
        console.log("Chain ID:              ", block.chainid);
        require(block.chainid == 271828, "Refusing to deploy on chain != 271828");

        vm.startBroadcast();
        deployed = new VoteEscrow(owner_, attestor, creator, guardian, minWeightToCreate);
        vm.stopBroadcast();

        console.log("Deployed at:           ", address(deployed));
        console.log("");
        console.log("Next steps:");
        console.log("1. If owner_ != DCSwapTimelock, schedule + execute transferOwnership(timelock).");
        console.log("2. Set VOTE_ESCROW_ADDRESS in rope-explorer's .env so /api/v1/votes/* can read it.");
        console.log("3. rope-explorer's cross-chain aggregator begins signing weight attestations");
        console.log("   for this contract address + chain 271828 (domain-bound, see WEIGHT_DOMAIN_TAG).");
    }
}
