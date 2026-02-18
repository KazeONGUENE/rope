/**
 * deploy_all.ts — Full ONCHAINID + T-REX + Custom Compliance Stack
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";
import * as fs from "fs";
import * as path from "path";

const TREX = path.resolve(__dirname, "../node_modules/@tokenysolutions/t-rex/artifacts/contracts");
function loadTrex(p: string) {
  return JSON.parse(fs.readFileSync(path.join(TREX, p), "utf-8"));
}

const TOPICS: Record<string, number> = {
  KYC_VALIDATED: 1, AML_VALIDATED: 2, COUNTRY: 3,
  ACCREDITED_INVESTOR: 4, DCNFT_HOLDER: 10, SOVEREIGN_IDENTITY: 99,
};

async function main() {
  const [deployer] = await ethers.getSigners();
  const network = await ethers.provider.getNetwork();
  const balance = await ethers.provider.getBalance(deployer.address);

  console.log("╔════════════════════════════════════════════════════════════╗");
  console.log("║  DATACHAIN ROPE — FULL DEPLOYMENT (Chain ID: 271828)      ║");
  console.log("╠════════════════════════════════════════════════════════════╣");
  console.log(`║  Deployer: ${deployer.address}`);
  console.log(`║  Balance:  ${ethers.formatEther(balance)} FAT`);
  console.log("╚════════════════════════════════════════════════════════════╝\n");

  const d: Record<string, string> = {};

  // [1] ClaimTopicsRegistry
  console.log("[1/7] ClaimTopicsRegistry...");
  const ctrA = loadTrex("registry/implementation/ClaimTopicsRegistry.sol/ClaimTopicsRegistry.json");
  const ctr = await (new ethers.ContractFactory(ctrA.abi, ctrA.bytecode, deployer)).deploy();
  await ctr.waitForDeployment();
  d.ClaimTopicsRegistry = await ctr.getAddress();
  await (await ctr.getFunction("init")()).wait();
  console.log("  Deployed + initialized:", d.ClaimTopicsRegistry);
  for (const [name, id] of Object.entries(TOPICS)) {
    await (await ctr.getFunction("addClaimTopic")(id)).wait();
    console.log(`  + ${name} (${id})`);
  }

  // [2] TrustedIssuersRegistry
  console.log("\n[2/7] TrustedIssuersRegistry...");
  const tirA = loadTrex("registry/implementation/TrustedIssuersRegistry.sol/TrustedIssuersRegistry.json");
  const tir = await (new ethers.ContractFactory(tirA.abi, tirA.bytecode, deployer)).deploy();
  await tir.waitForDeployment();
  d.TrustedIssuersRegistry = await tir.getAddress();
  await (await tir.getFunction("init")()).wait();
  console.log("  Deployed + initialized:", d.TrustedIssuersRegistry);

  // [3] IdentityRegistryStorage
  console.log("\n[3/7] IdentityRegistryStorage...");
  const irsA = loadTrex("registry/implementation/IdentityRegistryStorage.sol/IdentityRegistryStorage.json");
  const irs = await (new ethers.ContractFactory(irsA.abi, irsA.bytecode, deployer)).deploy();
  await irs.waitForDeployment();
  d.IdentityRegistryStorage = await irs.getAddress();
  await (await irs.getFunction("init")()).wait();
  console.log("  Deployed + initialized:", d.IdentityRegistryStorage);

  // [4] IdentityRegistry
  console.log("\n[4/7] IdentityRegistry...");
  const irA = loadTrex("registry/implementation/IdentityRegistry.sol/IdentityRegistry.json");
  const ir = await (new ethers.ContractFactory(irA.abi, irA.bytecode, deployer)).deploy();
  await ir.waitForDeployment();
  d.IdentityRegistry = await ir.getAddress();
  await (await ir.getFunction("init")(d.TrustedIssuersRegistry, d.ClaimTopicsRegistry, d.IdentityRegistryStorage)).wait();
  console.log("  Deployed + initialized:", d.IdentityRegistry);
  await (await irs.getFunction("bindIdentityRegistry")(d.IdentityRegistry)).wait();
  console.log("  + Storage bound to Registry");

  // [5] DatawalletClaimIssuer
  console.log("\n[5/7] DatawalletClaimIssuer...");
  const DCI = await ethers.getContractFactory("DatawalletClaimIssuer");
  const dci = await DCI.deploy(deployer.address, deployer.address);
  await dci.waitForDeployment();
  d.DatawalletClaimIssuer = await dci.getAddress();
  console.log("  Deployed:", d.DatawalletClaimIssuer);

  // Register as trusted issuer
  await (await tir.getFunction("addTrustedIssuer")(d.DatawalletClaimIssuer, Object.values(TOPICS))).wait();
  console.log("  + Registered as trusted issuer for all topics");

  // [6] RopeComplianceModule
  console.log("\n[6/7] RopeComplianceModule...");
  const RCM = await ethers.getContractFactory("RopeComplianceModule");
  const rcm = await RCM.deploy(d.IdentityRegistry, deployer.address);
  await rcm.waitForDeployment();
  d.RopeComplianceModule = await rcm.getAddress();
  console.log("  Deployed:", d.RopeComplianceModule);

  // [7] Summary
  const finalBal = await ethers.provider.getBalance(deployer.address);
  const gasUsed = balance - finalBal;

  console.log("\n╔════════════════════════════════════════════════════════════╗");
  console.log("║  DEPLOYMENT COMPLETE                                      ║");
  console.log("╠════════════════════════════════════════════════════════════╣");
  for (const [name, addr] of Object.entries(d)) {
    console.log(`║  ${name.padEnd(28)} ${addr}`);
  }
  console.log("╠════════════════════════════════════════════════════════════╣");
  console.log(`║  Gas: ${ethers.formatEther(gasUsed)} FAT`);
  console.log(`║  Remaining: ${ethers.formatEther(finalBal)} FAT`);
  console.log("╚════════════════════════════════════════════════════════════╝");

  fs.writeFileSync(path.resolve(__dirname, "../deployed_addresses.json"), JSON.stringify(d, null, 2));
  console.log("\nAddresses written to deployed_addresses.json");
}

main().then(() => process.exit(0)).catch((e) => { console.error("FAILED:", e); process.exit(1); });
