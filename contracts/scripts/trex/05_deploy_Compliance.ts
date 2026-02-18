/**
 * 05_deploy_Compliance.ts
 *
 * Deploys the ModularCompliance contract and binds the RopeComplianceModule
 * as the primary compliance module.  Also deploys the DatawalletClaimIssuer.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

async function main() {
  const [deployer] = await ethers.getSigners();

  const IDENTITY_REGISTRY = process.env.IDENTITY_REGISTRY_ADDRESS;
  if (!IDENTITY_REGISTRY) {
    throw new Error("IDENTITY_REGISTRY_ADDRESS env var required");
  }

  console.log("Deploying compliance stack with:", deployer.address);

  // -------------------------------------------------------------------------
  // 1. Deploy DatawalletClaimIssuer
  // -------------------------------------------------------------------------
  const DatawalletClaimIssuer = await ethers.getContractFactory("DatawalletClaimIssuer");
  const claimIssuer = await DatawalletClaimIssuer.deploy(
    deployer.address, // signing key (replace with HSM address in prod)
    deployer.address  // admin
  );
  await claimIssuer.waitForDeployment();
  const claimIssuerAddr = await claimIssuer.getAddress();
  console.log("DatawalletClaimIssuer deployed at:", claimIssuerAddr);

  // -------------------------------------------------------------------------
  // 2. Deploy RopeComplianceModule
  // -------------------------------------------------------------------------
  const RopeComplianceModule = await ethers.getContractFactory("RopeComplianceModule");
  const complianceModule = await RopeComplianceModule.deploy(
    IDENTITY_REGISTRY,
    deployer.address
  );
  await complianceModule.waitForDeployment();
  const complianceModuleAddr = await complianceModule.getAddress();
  console.log("RopeComplianceModule deployed at:", complianceModuleAddr);

  console.log("\n--- Compliance Stack Deployed ---");
  console.log("DatawalletClaimIssuer:", claimIssuerAddr);
  console.log("RopeComplianceModule: ", complianceModuleAddr);
  console.log("IdentityRegistry:     ", IDENTITY_REGISTRY);
  console.log("--------------------------------\n");

  return {
    claimIssuer: claimIssuerAddr,
    complianceModule: complianceModuleAddr,
  };
}

main()
  .then((addresses) => {
    console.log(JSON.stringify(addresses, null, 2));
    process.exit(0);
  })
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
