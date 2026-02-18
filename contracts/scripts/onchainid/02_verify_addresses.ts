/**
 * 02_verify_addresses.ts
 *
 * Verifies that the ONCHAINID IdFactory and related infrastructure are
 * deployed at the expected canonical addresses on Datachain Rope, ensuring
 * cross-chain interoperability with Ethereum mainnet.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

async function main() {
  const EXPECTED_FACTORY = process.env.ONCHAINID_FACTORY_ADDRESS || "";
  if (!EXPECTED_FACTORY) {
    console.warn("ONCHAINID_FACTORY_ADDRESS not set — running basic checks only.");
  }

  const [deployer] = await ethers.getSigners();
  const network = await ethers.provider.getNetwork();
  console.log("Verifying on chain", network.chainId);

  // -------------------------------------------------------------------------
  // 1. Check IdFactory bytecode is present
  // -------------------------------------------------------------------------
  if (EXPECTED_FACTORY) {
    const code = await ethers.provider.getCode(EXPECTED_FACTORY);
    if (code === "0x") {
      console.error("FAIL: No bytecode at IdFactory address", EXPECTED_FACTORY);
      process.exit(1);
    }
    console.log("PASS: IdFactory bytecode found at", EXPECTED_FACTORY);

    // -----------------------------------------------------------------------
    // 2. Verify IdFactory responds to getIdentity()
    // -----------------------------------------------------------------------
    const idFactory = await ethers.getContractAt("IdFactory", EXPECTED_FACTORY);
    try {
      const identity = await idFactory.getIdentity(deployer.address);
      console.log("PASS: getIdentity() callable. Deployer identity:", identity);
    } catch (e) {
      console.warn("WARN: getIdentity() reverted — deployer may not have an identity yet.");
    }
  }

  // -------------------------------------------------------------------------
  // 3. Verify DatawalletClaimIssuer
  // -------------------------------------------------------------------------
  const CLAIM_ISSUER = process.env.DATAWALLET_CLAIM_ISSUER_ADDRESS || "";
  if (CLAIM_ISSUER) {
    const code = await ethers.provider.getCode(CLAIM_ISSUER);
    if (code === "0x") {
      console.error("FAIL: No bytecode at ClaimIssuer address", CLAIM_ISSUER);
      process.exit(1);
    }
    console.log("PASS: DatawalletClaimIssuer bytecode found at", CLAIM_ISSUER);

    const issuer = await ethers.getContractAt("DatawalletClaimIssuer", CLAIM_ISSUER);
    const topics = await issuer.supportedTopics();
    console.log("PASS: Supported topics:", topics.map(Number));
  }

  // -------------------------------------------------------------------------
  // 4. Verify T-REX registry stack
  // -------------------------------------------------------------------------
  const registries = {
    ClaimTopicsRegistry: process.env.CLAIM_TOPICS_REGISTRY_ADDRESS,
    TrustedIssuersRegistry: process.env.TRUSTED_ISSUERS_REGISTRY_ADDRESS,
    IdentityRegistryStorage: process.env.IDENTITY_REGISTRY_STORAGE_ADDRESS,
    IdentityRegistry: process.env.IDENTITY_REGISTRY_ADDRESS,
  };

  for (const [name, address] of Object.entries(registries)) {
    if (!address) {
      console.warn(`SKIP: ${name} address not set.`);
      continue;
    }
    const code = await ethers.provider.getCode(address);
    if (code === "0x") {
      console.error(`FAIL: No bytecode at ${name} address ${address}`);
      process.exit(1);
    }
    console.log(`PASS: ${name} bytecode found at ${address}`);
  }

  console.log("\n--- All address verifications complete ---\n");
}

main()
  .then(() => process.exit(0))
  .catch((error) => {
    console.error("Verification failed:", error);
    process.exit(1);
  });
