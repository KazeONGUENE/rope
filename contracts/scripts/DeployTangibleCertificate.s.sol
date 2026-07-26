// SPDX-License-Identifier: UNLICENSED
pragma solidity ^0.8.20;

import "forge-std/Script.sol";
import "forge-std/console.sol";
import "../src/tangible/CertificateLifecycle.sol";
import "../src/tangible/RoyaltySplitter.sol";
import "../src/tangible/DCNFTDeed.sol";

/**
 * @title DeployTangibleCertificate
 * @notice Deploys the Tangible DC pre-order certificate stack to Datachain Rope
 *         (chainId 271828): CertificateLifecycle (lock/unlock + anchor),
 *         RoyaltySplitter (EIP-2981 network/buyer split) and DCNFTDeed
 *         (locked ERC-721 deed with transfer guard).
 *
 * USAGE (mainnet):
 *
 *   forge script scripts/DeployTangibleCertificate.s.sol \
 *     --rpc-url https://erpc.datachain.network \
 *     --private-key $DEPLOYER_PRIVATE_KEY \
 *     --broadcast --slow --legacy
 *
 * REQUIRED ENVIRONMENT VARIABLES:
 *   TC_ADMIN              DEFAULT_ADMIN_ROLE - foundation governance multisig
 *   TC_OPERATOR           ANCHOR_ROLE + OPERATOR_ROLE + MINTER_ROLE - backend signer
 *   TC_NETWORK_TREASURY   Datachain-network royalty share recipient
 *   DEPLOYER_PRIVATE_KEY  deployer EOA key
 * OPTIONAL:
 *   TC_NETWORK_BPS        default 300 (3.00%)
 *   TC_BUYER_BPS          default 200 (2.00%)
 *   TC_DEED_NAME          default "Tangible DC Certificate"
 *   TC_DEED_SYMBOL        default "TDC-CERT"
 */
contract DeployTangibleCertificate is Script {
    function run() external {
        address admin = vm.envAddress("TC_ADMIN");
        address operator = vm.envAddress("TC_OPERATOR");
        address treasury = vm.envAddress("TC_NETWORK_TREASURY");
        uint96 networkBps = uint96(vm.envOr("TC_NETWORK_BPS", uint256(300)));
        uint96 buyerBps = uint96(vm.envOr("TC_BUYER_BPS", uint256(200)));
        string memory deedName = vm.envOr("TC_DEED_NAME", string("Tangible DC Certificate"));
        string memory deedSymbol = vm.envOr("TC_DEED_SYMBOL", string("TDC-CERT"));

        require(admin != address(0), "deploy: admin=0");
        require(operator != address(0), "deploy: operator=0");
        require(treasury != address(0), "deploy: treasury=0");

        console.log("=================================================");
        console.log("  TANGIBLE DC CERTIFICATE DEPLOY - chainId 271828");
        console.log("=================================================");
        console.log("Admin    :", admin);
        console.log("Operator :", operator);
        console.log("Treasury :", treasury);
        console.log("Royalty  : network/buyer bps", networkBps, buyerBps);

        uint256 pk = vm.envUint("DEPLOYER_PRIVATE_KEY");
        vm.startBroadcast(pk);

        CertificateLifecycle lifecycle = new CertificateLifecycle(admin);
        RoyaltySplitter splitter = new RoyaltySplitter(admin, treasury, networkBps, buyerBps);
        DCNFTDeed deed = new DCNFTDeed(deedName, deedSymbol, admin, address(lifecycle));

        // Grant the backend signer the operational roles. Admin keeps governance.
        if (operator != admin) {
            lifecycle.grantRole(lifecycle.ANCHOR_ROLE(), operator);
            lifecycle.grantRole(lifecycle.OPERATOR_ROLE(), operator);
            splitter.grantRole(splitter.SPLIT_ADMIN_ROLE(), operator);
            deed.grantRole(deed.MINTER_ROLE(), operator);
        }

        vm.stopBroadcast();

        console.log("");
        console.log("CertificateLifecycle:", address(lifecycle));
        console.log("RoyaltySplitter     :", address(splitter));
        console.log("DCNFTDeed           :", address(deed));
        console.log("");
        console.log("NEXT STEPS:");
        console.log("  1. Set platform env: ROPE_ANCHOR_CONTRACT, DCNFT_CONTRACT, ROYALTY_RECIPIENT.");
        console.log("  2. Verify each contract on dcscan via forge verify-contract.");
        console.log("  3. Set TDC_DRYRUN=0 and run an end-to-end paid order on testnet first.");
    }
}
