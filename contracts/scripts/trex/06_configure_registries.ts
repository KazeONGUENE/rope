/**
 * 06_configure_registries.ts
 *
 * Final configuration step: wires together all T-REX registries, registers
 * Datawallet+ as trusted issuer, and sets initial compliance rules.
 *
 * Run after steps 01–05 have completed and all addresses are available.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";

async function main() {
  const [deployer] = await ethers.getSigners();

  const CLAIM_TOPICS_REG = process.env.CLAIM_TOPICS_REGISTRY_ADDRESS;
  const TRUSTED_ISSUERS_REG = process.env.TRUSTED_ISSUERS_REGISTRY_ADDRESS;
  const IDENTITY_REG = process.env.IDENTITY_REGISTRY_ADDRESS;
  const CLAIM_ISSUER = process.env.DATAWALLET_CLAIM_ISSUER_ADDRESS;
  const COMPLIANCE_MODULE = process.env.ROPE_COMPLIANCE_MODULE_ADDRESS;

  if (!CLAIM_TOPICS_REG || !TRUSTED_ISSUERS_REG || !IDENTITY_REG || !CLAIM_ISSUER) {
    throw new Error(
      "Required: CLAIM_TOPICS_REGISTRY_ADDRESS, TRUSTED_ISSUERS_REGISTRY_ADDRESS, " +
        "IDENTITY_REGISTRY_ADDRESS, DATAWALLET_CLAIM_ISSUER_ADDRESS"
    );
  }

  console.log("Configuring T-REX registries…");

  // -------------------------------------------------------------------------
  // 1. Verify claim topics
  // -------------------------------------------------------------------------
  const claimTopics = await ethers.getContractAt("ClaimTopicsRegistry", CLAIM_TOPICS_REG);
  const topics = await claimTopics.getClaimTopics();
  console.log("Claim topics registered:", topics.map(Number));

  // -------------------------------------------------------------------------
  // 2. Verify trusted issuer registration
  // -------------------------------------------------------------------------
  const trustedIssuers = await ethers.getContractAt(
    "TrustedIssuersRegistry",
    TRUSTED_ISSUERS_REG
  );
  const isTrusted = await trustedIssuers.isTrustedIssuer(CLAIM_ISSUER);
  if (!isTrusted) {
    console.log("Registering Datawallet+ as trusted issuer…");
    const tx = await trustedIssuers.addTrustedIssuer(CLAIM_ISSUER, [1, 2, 3, 4, 10, 99]);
    await tx.wait();
    console.log("Done.");
  } else {
    console.log("Datawallet+ already registered as trusted issuer.");
  }

  // -------------------------------------------------------------------------
  // 3. Configure RopeComplianceModule default rules
  // -------------------------------------------------------------------------
  if (COMPLIANCE_MODULE) {
    const compliance = await ethers.getContractAt("RopeComplianceModule", COMPLIANCE_MODULE);

    // Restricted countries (DPRK, Iran, Syria, Cuba, Crimea)
    const restricted = [408, 364, 760, 192, 804];
    for (const code of restricted) {
      const tx = await compliance.setRestrictedCountry(code, true);
      await tx.wait();
      console.log(`  Restricted country ${code}`);
    }

    // Default lockup: 90 days
    const NINETY_DAYS = 90 * 24 * 60 * 60;
    const txLockup = await compliance.setLockupPeriod(NINETY_DAYS);
    await txLockup.wait();
    console.log("  Lockup period: 90 days");
  }

  console.log("\n--- Registry Configuration Complete ---\n");
}

main()
  .then(() => process.exit(0))
  .catch((err) => {
    console.error(err);
    process.exit(1);
  });
