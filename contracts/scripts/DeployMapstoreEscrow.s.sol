// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "forge-std/console.sol";
import "../src/mapstore/MapstoreEscrow.sol";

/**
 * @title DeployMapstoreEscrow
 * @notice Deploys `MapstoreEscrow` to Datachain Rope (chainId 271828).
 *
 * USAGE (mainnet):
 *
 *   forge script scripts/DeployMapstoreEscrow.s.sol \
 *     --rpc-url https://erpc.datachain.network \
 *     --private-key $DEPLOYER_PRIVATE_KEY \
 *     --broadcast --slow \
 *     --legacy
 *
 * REQUIRED ENVIRONMENT VARIABLES (set by the Datachain Rope operator just
 * before running the broadcast, AFTER the Mapstore team has confirmed all
 * five governance addresses):
 *
 *   PLATFORM_TREASURY  Mapstore treasury wallet (will receive the platform fee)
 *   ESCROW_ADMIN       DEFAULT_ADMIN_ROLE - Mapstore governance multisig
 *   ESCROW_PLATFORM    PLATFORM_ROLE      - Mapstore API relayer EOA
 *   ESCROW_OPERATOR    OPERATOR_ROLE      - Mapstore operator multisig (disputes)
 *   ESCROW_GUARDIAN    GUARDIAN_ROLE      - Mapstore guardian multisig (pauser)
 *   DEPLOYER_PRIVATE_KEY  Signing key of the deployer EOA (foundation wallet)
 *
 * VERIFICATION (after broadcast):
 *
 *   forge verify-contract <ADDRESS> src/mapstore/MapstoreEscrow.sol:MapstoreEscrow \
 *     --chain 271828 \
 *     --verifier-url https://api.dcscan.io/api \
 *     --etherscan-api-key $ETHERSCAN_API_KEY \
 *     --constructor-args $(cast abi-encode "constructor(address,address,address,address,address)" \
 *       $PLATFORM_TREASURY $ESCROW_ADMIN $ESCROW_PLATFORM $ESCROW_OPERATOR $ESCROW_GUARDIAN)
 */
contract DeployMapstoreEscrow is Script {
    function run() external {
        // Load env. vm.envAddress reverts if the var is missing, which is
        // exactly what we want - no silent default-to-zero deploys.
        address platformTreasury = vm.envAddress("PLATFORM_TREASURY");
        address escrowAdmin      = vm.envAddress("ESCROW_ADMIN");
        address escrowPlatform   = vm.envAddress("ESCROW_PLATFORM");
        address escrowOperator   = vm.envAddress("ESCROW_OPERATOR");
        address escrowGuardian   = vm.envAddress("ESCROW_GUARDIAN");

        // Anti-foot-gun: make sure no two roles collapse to the same address
        // unintentionally. A real multisig setup MUST keep them separate.
        require(platformTreasury != address(0), "deploy: treasury=0");
        require(escrowAdmin     != address(0), "deploy: admin=0");
        require(escrowPlatform  != address(0), "deploy: platform=0");
        require(escrowOperator  != address(0), "deploy: operator=0");
        require(escrowGuardian  != address(0), "deploy: guardian=0");
        require(escrowAdmin     != escrowPlatform, "deploy: admin==platform");
        require(escrowAdmin     != escrowOperator, "deploy: admin==operator");
        require(escrowOperator  != escrowGuardian, "deploy: operator==guardian");

        console.log("==============================================");
        console.log("  MAPSTORE ESCROW DEPLOYMENT - chainId 271828");
        console.log("==============================================");
        console.log("Platform treasury:", platformTreasury);
        console.log("Admin (DEFAULT_ADMIN):", escrowAdmin);
        console.log("Platform relayer:", escrowPlatform);
        console.log("Operator (disputes):", escrowOperator);
        console.log("Guardian (pauser):", escrowGuardian);
        console.log("Deployer balance:", msg.sender.balance);

        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        vm.startBroadcast(pk);

        MapstoreEscrow escrow = new MapstoreEscrow(
            platformTreasury,
            escrowAdmin,
            escrowPlatform,
            escrowOperator,
            escrowGuardian
        );

        vm.stopBroadcast();

        console.log("");
        console.log("MapstoreEscrow deployed at:", address(escrow));
        console.log("Default platform fee bps  :", escrow.defaultPlatformFeeBps());
        console.log("Default dispute window (s):", escrow.defaultDisputeWindow());
        console.log("");
        console.log("NEXT STEPS (Rope operator):");
        console.log("  1. Update deployed_addresses.json with the address above.");
        console.log("  2. Patch crates/rope-explorer/src/labels.rs::entity_labels::built_in()");
        console.log("     with the 6 labels (escrow + 5 governance addresses).");
        console.log("  3. Run deploy/scripts/deploy-fleet.sh full to roll out the dcscan");
        console.log("     labels to BLUE + GREEN + DO-1 + DO-2.");
        console.log("  4. Anchor a MapstoreEscrowEstablished knot on the treasury's");
        console.log("     personal ledger via rope_createPersonalLedger +");
        console.log("     rope_appendToLedger (see datachain-rope/docs/MAPSTORE_DEPLOY_RUNBOOK.md).");
    }
}
