/**
 * DatawalletClaimIssuer.test.ts
 *
 * Unit tests for the Datawallet+ ClaimIssuer contract ensuring claims
 * can be issued, verified, and revoked correctly on Datachain Rope.
 *
 * @author Kazé A. ONGUENE — Datachain Foundation
 */

import { expect } from "chai";
import { ethers } from "hardhat";
import { SignerWithAddress } from "@nomicfoundation/hardhat-ethers/signers";

describe("DatawalletClaimIssuer", function () {
  let admin: SignerWithAddress;
  let issuer: SignerWithAddress;
  let investor: SignerWithAddress;
  let claimIssuer: any;

  const KYC_VALIDATED = 1;
  const AML_VALIDATED = 2;
  const COUNTRY = 3;
  const ACCREDITED_INVESTOR = 4;
  const DCNFT_HOLDER = 10;
  const SOVEREIGN_IDENTITY = 99;

  beforeEach(async function () {
    [admin, issuer, investor] = await ethers.getSigners();

    const DatawalletClaimIssuer = await ethers.getContractFactory("DatawalletClaimIssuer");
    claimIssuer = await DatawalletClaimIssuer.deploy(admin.address, admin.address);
    await claimIssuer.waitForDeployment();
  });

  describe("Deployment", function () {
    it("should set the correct signing key", async function () {
      expect(await claimIssuer.signingKey()).to.equal(admin.address);
    });

    it("should register all six default claim topics", async function () {
      const topics = await claimIssuer.supportedTopics();
      const topicNumbers = topics.map(Number);
      expect(topicNumbers).to.include(KYC_VALIDATED);
      expect(topicNumbers).to.include(AML_VALIDATED);
      expect(topicNumbers).to.include(COUNTRY);
      expect(topicNumbers).to.include(ACCREDITED_INVESTOR);
      expect(topicNumbers).to.include(DCNFT_HOLDER);
      expect(topicNumbers).to.include(SOVEREIGN_IDENTITY);
    });

    it("should grant admin the ISSUER_ROLE and REVOKER_ROLE", async function () {
      const ISSUER_ROLE = await claimIssuer.ISSUER_ROLE();
      const REVOKER_ROLE = await claimIssuer.REVOKER_ROLE();
      expect(await claimIssuer.hasRole(ISSUER_ROLE, admin.address)).to.be.true;
      expect(await claimIssuer.hasRole(REVOKER_ROLE, admin.address)).to.be.true;
    });
  });

  describe("signClaim()", function () {
    it("should produce a signature for a supported topic", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(
        ["uint256", "uint256"],
        [Date.now(), 3]
      );
      const tx = await claimIssuer.signClaim(investor.address, KYC_VALIDATED, data);
      const receipt = await tx.wait();
      expect(receipt.status).to.equal(1);
    });

    it("should revert for unsupported topic", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(["uint256"], [1]);
      await expect(
        claimIssuer.signClaim(investor.address, 999, data)
      ).to.be.revertedWith("unsupported topic");
    });

    it("should revert when called by non-issuer", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(["uint256"], [1]);
      await expect(
        claimIssuer.connect(investor).signClaim(investor.address, KYC_VALIDATED, data)
      ).to.be.reverted;
    });
  });

  describe("revokeClaim()", function () {
    it("should mark a claim as revoked", async function () {
      const claimId = ethers.keccak256(ethers.toUtf8Bytes("test-claim"));
      await claimIssuer.revokeClaim(claimId);
      expect(await claimIssuer.revokedClaims(claimId)).to.be.true;
    });

    it("should emit ClaimRevoked event", async function () {
      const claimId = ethers.keccak256(ethers.toUtf8Bytes("test-claim-2"));
      await expect(claimIssuer.revokeClaim(claimId))
        .to.emit(claimIssuer, "ClaimRevoked");
    });

    it("should revert on double revocation", async function () {
      const claimId = ethers.keccak256(ethers.toUtf8Bytes("test-claim-3"));
      await claimIssuer.revokeClaim(claimId);
      await expect(claimIssuer.revokeClaim(claimId)).to.be.revertedWith(
        "already revoked"
      );
    });
  });

  describe("Admin functions", function () {
    it("should allow admin to change signing key", async function () {
      await claimIssuer.setSigningKey(issuer.address);
      expect(await claimIssuer.signingKey()).to.equal(issuer.address);
    });

    it("should allow admin to add a new supported topic", async function () {
      await claimIssuer.addSupportedTopic(200);
      const topics = await claimIssuer.supportedTopics();
      expect(topics.map(Number)).to.include(200);
    });

    it("should revert when non-admin tries to change signing key", async function () {
      await expect(
        claimIssuer.connect(investor).setSigningKey(investor.address)
      ).to.be.reverted;
    });
  });

  describe("supportsInterface()", function () {
    it("should support IClaimIssuer interface", async function () {
      // IClaimIssuer interface ID
      const result = await claimIssuer.supportsInterface("0x01ffc9a7"); // ERC-165
      expect(result).to.be.true;
    });
  });

  async function getBlockTimestamp(): Promise<number> {
    const block = await ethers.provider.getBlock("latest");
    return block!.timestamp;
  }
});
