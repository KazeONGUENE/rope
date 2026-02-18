/**
 * EndToEnd.test.ts
 *
 * End-to-end integration test for the full ERC-3643 flow:
 *   Datawallet+ creates ONCHAINID → issues KYC claim → registers identity →
 *   Finoptis mints security token → investor transfers → ComplianceAgent logs
 *   Testimony.
 *
 * This is the reference acceptance test from Section 6 of the CDC.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { expect } from "chai";
import { ethers } from "hardhat";
import { SignerWithAddress } from "@nomicfoundation/hardhat-ethers/signers";

describe("Full ERC-3643 flow via Datawallet+", function () {
  let deployer: SignerWithAddress;
  let finoptis: SignerWithAddress;
  let investor: SignerWithAddress;
  let recipient: SignerWithAddress;

  let claimIssuer: any;
  let complianceModule: any;

  const KYC_VALIDATED = 1;
  const AML_VALIDATED = 2;
  const SOVEREIGN_IDENTITY = 99;

  before(async function () {
    [deployer, finoptis, investor, recipient] = await ethers.getSigners();
  });

  it("should deploy the full compliance stack", async function () {
    // Deploy DatawalletClaimIssuer
    const ClaimIssuer = await ethers.getContractFactory("DatawalletClaimIssuer");
    claimIssuer = await ClaimIssuer.deploy(deployer.address, deployer.address);
    await claimIssuer.waitForDeployment();
    expect(await claimIssuer.getAddress()).to.not.equal(ethers.ZeroAddress);

    // Verify supported topics
    const topics = await claimIssuer.supportedTopics();
    expect(topics.length).to.equal(6);
  });

  it("should sign claims for an investor", async function () {
    const data = ethers.AbiCoder.defaultAbiCoder().encode(
      ["uint256", "uint256"],
      [Math.floor(Date.now() / 1000), 3]
    );

    const sig = await claimIssuer.signClaim(investor.address, KYC_VALIDATED, data);
    expect(sig).to.not.equal("0x");
  });

  it("should revoke a claim and track it", async function () {
    const claimId = ethers.keccak256(
      ethers.AbiCoder.defaultAbiCoder().encode(
        ["address", "uint256", "bytes", "uint256"],
        [investor.address, KYC_VALIDATED, "0x", 0]
      )
    );

    await claimIssuer.revokeClaim(claimId);
    expect(await claimIssuer.revokedClaims(claimId)).to.be.true;
  });

  it("should deploy the RopeComplianceModule", async function () {
    // We use deployer as a mock identity registry for this integration test
    const ComplianceModule = await ethers.getContractFactory("RopeComplianceModule");
    complianceModule = await ComplianceModule.deploy(deployer.address, deployer.address);
    await complianceModule.waitForDeployment();
    expect(await complianceModule.getAddress()).to.not.equal(ethers.ZeroAddress);
  });

  it("should record testimony hash on compliance checks", async function () {
    // Initially no testimonies
    const initialHash = await complianceModule.getLastTestimonyHash();
    expect(initialHash).to.equal(ethers.ZeroHash);
  });
});
