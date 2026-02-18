/**
 * 01_deploy_IdFactory.ts
 *
 * Deploys the ONCHAINID IdFactory and ImplementationAuthority to
 * Datachain Rope using pre-compiled artifacts from @onchain-id/solidity.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { ethers } from "hardhat";
import * as fs from "fs";
import * as path from "path";

function loadArtifact(contractPath: string) {
  const fullPath = path.resolve(
    __dirname, "../../node_modules/@onchain-id/solidity/artifacts/contracts",
    contractPath
  );
  return JSON.parse(fs.readFileSync(fullPath, "utf-8"));
}

async function main() {
  const [deployer] = await ethers.getSigners();
  const network = await ethers.provider.getNetwork();
  const balance = await ethers.provider.getBalance(deployer.address);

  console.log("╔════════════════════════════════════════════════════════════════╗");
  console.log("║       ONCHAINID DEPLOYMENT — DATACHAIN ROPE                    ║");
  console.log("╚════════════════════════════════════════════════════════════════╝");
  console.log("Deployer:", deployer.address);
  console.log("Chain ID:", network.chainId.toString());
  console.log("Balance:", ethers.formatEther(balance), "FAT");
  console.log("");

  // 1. Deploy IdFactory
  console.log("[1/3] Deploying IdFactory...");
  const idFactoryArtifact = loadArtifact("factory/IdFactory.sol/IdFactory.json");
  const IdFactory = new ethers.ContractFactory(
    idFactoryArtifact.abi, idFactoryArtifact.bytecode, deployer
  );
  const idFactory = await IdFactory.deploy(deployer.address);
  await idFactory.waitForDeployment();
  const idFactoryAddr = await idFactory.getAddress();
  console.log("  IdFactory deployed at:", idFactoryAddr);

  // 2. Deploy ImplementationAuthority
  console.log("[2/3] Deploying ImplementationAuthority...");
  const implAuthArtifact = loadArtifact(
    "proxy/ImplementationAuthority.sol/ImplementationAuthority.json"
  );
  const ImplAuth = new ethers.ContractFactory(
    implAuthArtifact.abi, implAuthArtifact.bytecode, deployer
  );
  const implAuth = await ImplAuth.deploy(idFactoryAddr);
  await implAuth.waitForDeployment();
  const implAuthAddr = await implAuth.getAddress();
  console.log("  ImplementationAuthority deployed at:", implAuthAddr);

  // 3. Create test identity
  console.log("[3/3] Creating test ONCHAINID identity...");
  const salt = "datachain-genesis-identity-" + Date.now();
  const tx = await idFactory.getFunction("createIdentity")(deployer.address, salt);
  const receipt = await tx.wait();
  const identityAddr = await idFactory.getFunction("getIdentity")(deployer.address);
  console.log("  Genesis identity created:", identityAddr);
  console.log("  Tx hash:", receipt?.hash);

  console.log("");
  console.log("╔════════════════════════════════════════════════════════════════╗");
  console.log("║  ONCHAINID DEPLOYMENT COMPLETE                                 ║");
  console.log("╠════════════════════════════════════════════════════════════════╣");
  console.log("║  IdFactory:               ", idFactoryAddr);
  console.log("║  ImplementationAuthority: ", implAuthAddr);
  console.log("║  Genesis Identity:        ", identityAddr);
  console.log("╚════════════════════════════════════════════════════════════════╝");

  return { idFactory: idFactoryAddr, implementationAuthority: implAuthAddr, genesisIdentity: identityAddr };
}

main()
  .then((addresses) => {
    console.log("\nSave these addresses to .env:");
    console.log(JSON.stringify(addresses, null, 2));
    process.exit(0);
  })
  .catch((error) => {
    console.error("Deployment failed:", error);
    process.exit(1);
  });
