/**
 * 04_deploy_IdentityRegistry.ts
 *
 * Deploys the IdentityRegistry and binds it to the three registries deployed
 * in steps 01-03.  This contract is the core of T-REX identity verification:
 * every ERC-3643 transfer calls isVerified() here.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

async function main() {
  const [deployer] = await ethers.getSigners();

  const TRUSTED_ISSUERS = process.env.TRUSTED_ISSUERS_REGISTRY_ADDRESS;
  const CLAIM_TOPICS = process.env.CLAIM_TOPICS_REGISTRY_ADDRESS;
  const STORAGE = process.env.IDENTITY_REGISTRY_STORAGE_ADDRESS;

  if (!TRUSTED_ISSUERS || !CLAIM_TOPICS || !STORAGE) {
    throw new Error(
      "Required env vars: TRUSTED_ISSUERS_REGISTRY_ADDRESS, " +
        "CLAIM_TOPICS_REGISTRY_ADDRESS, IDENTITY_REGISTRY_STORAGE_ADDRESS"
    );
  }

  console.log("Deploying IdentityRegistry with:", deployer.address);
  console.log("  TrustedIssuers:", TRUSTED_ISSUERS);
  console.log("  ClaimTopics:   ", CLAIM_TOPICS);
  console.log("  Storage:       ", STORAGE);

  const IdentityRegistry = await ethers.getContractFactory("IdentityRegistry");
  const registry = await IdentityRegistry.deploy(
    TRUSTED_ISSUERS,
    CLAIM_TOPICS,
    STORAGE
  );
  await registry.waitForDeployment();
  const address = await registry.getAddress();
  console.log("IdentityRegistry deployed at:", address);

  // Bind storage to this registry
  const storageContract = await ethers.getContractAt("IdentityRegistryStorage", STORAGE);
  const tx = await storageContract.bindIdentityRegistry(address);
  await tx.wait();
  console.log("IdentityRegistryStorage bound to IdentityRegistry");

  return address;
}

main()
  .then((addr) => {
    console.log("IDENTITY_REGISTRY_ADDRESS=" + addr);
    process.exit(0);
  })
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
