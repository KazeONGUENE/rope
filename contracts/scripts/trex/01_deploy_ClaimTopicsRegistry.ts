/**
 * 01_deploy_ClaimTopicsRegistry.ts
 *
 * Deploys the ClaimTopicsRegistry and registers the six standard claim topics
 * used across the Datachain / Datawallet+ / Finoptis ecosystem.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

const TOPICS = {
  KYC_VALIDATED: 1,
  AML_VALIDATED: 2,
  COUNTRY: 3,
  ACCREDITED_INVESTOR: 4,
  DCNFT_HOLDER: 10,
  SOVEREIGN_IDENTITY: 99,
};

async function main() {
  const [deployer] = await ethers.getSigners();
  console.log("Deploying ClaimTopicsRegistry with:", deployer.address);

  const ClaimTopicsRegistry = await ethers.getContractFactory("ClaimTopicsRegistry");
  const registry = await ClaimTopicsRegistry.deploy();
  await registry.waitForDeployment();
  const address = await registry.getAddress();
  console.log("ClaimTopicsRegistry deployed at:", address);

  for (const [name, topic] of Object.entries(TOPICS)) {
    const tx = await registry.addClaimTopic(topic);
    await tx.wait();
    console.log(`  Registered topic ${topic} (${name})`);
  }

  console.log("\nClaimTopicsRegistry ready.");
  return address;
}

main()
  .then((addr) => {
    console.log("CLAIM_TOPICS_REGISTRY_ADDRESS=" + addr);
    process.exit(0);
  })
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
