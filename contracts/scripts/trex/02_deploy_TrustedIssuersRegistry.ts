/**
 * 02_deploy_TrustedIssuersRegistry.ts
 *
 * Deploys the TrustedIssuersRegistry and registers Datawallet+ as a
 * trusted claim issuer for all supported topics.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

const ALL_TOPICS = [1, 2, 3, 4, 10, 99];

async function main() {
  const [deployer] = await ethers.getSigners();
  const DATAWALLET_ISSUER = process.env.DATAWALLET_CLAIM_ISSUER_ADDRESS;
  if (!DATAWALLET_ISSUER) {
    throw new Error("DATAWALLET_CLAIM_ISSUER_ADDRESS env var required");
  }

  console.log("Deploying TrustedIssuersRegistry with:", deployer.address);

  const TrustedIssuersRegistry = await ethers.getContractFactory("TrustedIssuersRegistry");
  const registry = await TrustedIssuersRegistry.deploy();
  await registry.waitForDeployment();
  const address = await registry.getAddress();
  console.log("TrustedIssuersRegistry deployed at:", address);

  const tx = await registry.addTrustedIssuer(DATAWALLET_ISSUER, ALL_TOPICS);
  await tx.wait();
  console.log("Datawallet+ registered as trusted issuer for topics:", ALL_TOPICS);

  return address;
}

main()
  .then((addr) => {
    console.log("TRUSTED_ISSUERS_REGISTRY_ADDRESS=" + addr);
    process.exit(0);
  })
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
