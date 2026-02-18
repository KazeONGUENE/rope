/**
 * 01_deploy_IdFactory.ts
 *
 * Deploys the ONCHAINID IdFactory to a deterministic CREATE2 address on
 * Datachain Rope so that identity addresses are interoperable with Ethereum
 * and Polygon.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

// Claim topic constants
const KYC_VALIDATED = 1;
const AML_VALIDATED = 2;
const COUNTRY = 3;
const ACCREDITED_INVESTOR = 4;
const DCNFT_HOLDER = 10;
const SOVEREIGN_IDENTITY = 99;

async function main() {
  const [deployer] = await ethers.getSigners();
  console.log("Deploying ONCHAINID IdFactory with account:", deployer.address);
  console.log("Network:", (await ethers.provider.getNetwork()).chainId);
  console.log("Balance:", ethers.formatEther(await ethers.provider.getBalance(deployer.address)));

  // -------------------------------------------------------------------------
  // 1. Deploy IdFactory via CREATE2 for deterministic addressing
  // -------------------------------------------------------------------------
  const IdFactory = await ethers.getContractFactory("IdFactory");
  const idFactory = await IdFactory.deploy(deployer.address);
  await idFactory.waitForDeployment();
  const idFactoryAddress = await idFactory.getAddress();
  console.log("IdFactory deployed at:", idFactoryAddress);

  // -------------------------------------------------------------------------
  // 2. Deploy ImplementationAuthority (beacon for ONCHAINID proxies)
  // -------------------------------------------------------------------------
  const ImplementationAuthority = await ethers.getContractFactory("ImplementationAuthority");
  const implAuth = await ImplementationAuthority.deploy(idFactoryAddress);
  await implAuth.waitForDeployment();
  console.log("ImplementationAuthority deployed at:", await implAuth.getAddress());

  // -------------------------------------------------------------------------
  // 3. Verify deployment
  // -------------------------------------------------------------------------
  console.log("\n--- ONCHAINID Infrastructure Deployed ---");
  console.log("IdFactory:               ", idFactoryAddress);
  console.log("ImplementationAuthority: ", await implAuth.getAddress());
  console.log("Deployer:                ", deployer.address);
  console.log("Chain ID:                ", (await ethers.provider.getNetwork()).chainId);
  console.log("-----------------------------------------\n");

  // -------------------------------------------------------------------------
  // 4. Create a test identity (optional, for verification)
  // -------------------------------------------------------------------------
  const testSalt = "datawallet-test-identity-" + Date.now();
  const saltTaken = await idFactory.isSaltTaken(
    ethers.keccak256(ethers.toUtf8Bytes(testSalt))
  );

  if (!saltTaken) {
    const tx = await idFactory.createIdentity(deployer.address, testSalt);
    const receipt = await tx.wait();
    console.log("Test identity created. Tx:", receipt?.hash);

    const identityAddress = await idFactory.getIdentity(deployer.address);
    console.log("Test ONCHAINID address:", identityAddress);
  }

  return {
    idFactory: idFactoryAddress,
    implementationAuthority: await implAuth.getAddress(),
  };
}

main()
  .then((addresses) => {
    console.log("\nDeployment complete. Save these addresses:");
    console.log(JSON.stringify(addresses, null, 2));
    process.exit(0);
  })
  .catch((error) => {
    console.error("Deployment failed:", error);
    process.exit(1);
  });
