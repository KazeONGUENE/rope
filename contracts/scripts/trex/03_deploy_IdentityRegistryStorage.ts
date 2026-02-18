/**
 * 03_deploy_IdentityRegistryStorage.ts
 *
 * Deploys the IdentityRegistryStorage contract that holds the mapping
 * between wallet addresses and ONCHAINID identity contracts.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

async function main() {
  const [deployer] = await ethers.getSigners();
  console.log("Deploying IdentityRegistryStorage with:", deployer.address);

  const IdentityRegistryStorage = await ethers.getContractFactory("IdentityRegistryStorage");
  const storage = await IdentityRegistryStorage.deploy();
  await storage.waitForDeployment();
  const address = await storage.getAddress();
  console.log("IdentityRegistryStorage deployed at:", address);

  return address;
}

main()
  .then((addr) => {
    console.log("IDENTITY_REGISTRY_STORAGE_ADDRESS=" + addr);
    process.exit(0);
  })
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
