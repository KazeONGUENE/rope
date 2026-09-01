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
    // 2026-07-26 counter-audit fix: signClaim() is now a pure `view` digest
    // helper (a contract cannot itself hold/use signingKey's private key),
    // so it resolves directly to the returned bytes instead of a mined
    // transaction — no more tx.wait().
    it("should return the digest to be signed off-chain for a supported topic", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(
        ["uint256", "uint256"],
        [Date.now(), 3]
      );
      const digest = await claimIssuer.signClaim(investor.address, KYC_VALIDATED, data);
      const expected = await claimIssuer.claimDigest(investor.address, KYC_VALIDATED, data);
      expect(digest).to.equal(expected);
      expect(ethers.dataLength(digest)).to.equal(32);
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

  describe("issueClaimToIdentity() / isClaimValid() — 2026-07-26 counter-audit fix", function () {
    // signingKey was set to `admin.address` in beforeEach(); sign with the
    // matching Hardhat private key so ecrecover(...) == signingKey.
    async function signDigest(digest: string): Promise<string> {
      return admin.signMessage(ethers.getBytes(digest));
    }

    it("should issue a claim with a real signingKey-recoverable signature", async function () {
      // issueClaimToIdentity() ends by calling identity.addClaim(...), which
      // per ERC-735 is `onlyClaimKey` — the claimIssuer contract itself must
      // hold a CLAIM_SIGNER_KEY (purpose 3) on the target Identity. Deploy a
      // real @onchain-id/solidity Identity for `investor` and grant the
      // claimIssuer contract that key, exactly as Datawallet+ onboarding
      // would (the identity owner delegates claim-writing to the issuer it
      // trusts).
      const Identity = await ethers.getContractFactory("Identity");
      const identity = await Identity.deploy(investor.address, false);
      await identity.waitForDeployment();
      const claimIssuerAddress = await claimIssuer.getAddress();
      const claimIssuerKey = ethers.keccak256(
        ethers.AbiCoder.defaultAbiCoder().encode(["address"], [claimIssuerAddress])
      );
      await identity.connect(investor).addKey(claimIssuerKey, 3, 1);

      const data = ethers.AbiCoder.defaultAbiCoder().encode(["uint256"], [1]);
      const identityAddress = await identity.getAddress();
      const digest = await claimIssuer.claimDigest(identityAddress, KYC_VALIDATED, data);
      const signature = await signDigest(digest);

      await expect(
        claimIssuer.issueClaimToIdentity(identityAddress, KYC_VALIDATED, data, signature)
      ).to.emit(claimIssuer, "ClaimIssued");
    });

    it("should reject a forged 32-byte digest-as-signature (the pre-fix exploit)", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(["uint256"], [2]);
      const digest = await claimIssuer.claimDigest(investor.address, KYC_VALIDATED, data);
      // Attacker who does NOT hold signingKey's private key, but can
      // compute the digest itself, submits the raw digest as "signature".
      await expect(
        claimIssuer.issueClaimToIdentity(investor.address, KYC_VALIDATED, data, digest)
      ).to.be.reverted;
    });

    it("should reject a signature from a non-signingKey wallet", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(["uint256"], [3]);
      const digest = await claimIssuer.claimDigest(investor.address, KYC_VALIDATED, data);
      const wrongSignature = await investor.signMessage(ethers.getBytes(digest));
      await expect(
        claimIssuer.issueClaimToIdentity(investor.address, KYC_VALIDATED, data, wrongSignature)
      ).to.be.revertedWith("invalid claim signature");
    });

    it("isClaimValid() should accept only a real 65-byte signingKey signature", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(["uint256"], [4]);
      const digest = await claimIssuer.claimDigest(investor.address, KYC_VALIDATED, data);
      const signature = await signDigest(digest);

      // Mock IIdentity is not needed: isClaimValid only reads _identity's
      // address, never calls into it.
      expect(
        await claimIssuer.isClaimValid(investor.address, KYC_VALIDATED, signature, data)
      ).to.be.true;

      // Pre-fix exploit: raw 32-byte digest accepted as "valid".
      expect(
        await claimIssuer.isClaimValid(investor.address, KYC_VALIDATED, digest, data)
      ).to.be.false;
    });

    it("revokeClaim() should invalidate the exact claim it was issued for", async function () {
      const data = ethers.AbiCoder.defaultAbiCoder().encode(["uint256"], [5]);
      const digest = await claimIssuer.claimDigest(investor.address, KYC_VALIDATED, data);
      const signature = await signDigest(digest);

      expect(
        await claimIssuer.isClaimValid(investor.address, KYC_VALIDATED, signature, data)
      ).to.be.true;

      // claimDigest has no timestamp/nonce component, so this is exactly
      // the id issueClaimToIdentity would have returned — the pre-fix bug
      // was that isClaimValid recomputed a DIFFERENT (always-zero-timestamp)
      // id, so revocation never actually blocked validation.
      await claimIssuer.revokeClaim(digest);

      expect(
        await claimIssuer.isClaimValid(investor.address, KYC_VALIDATED, signature, data)
      ).to.be.false;
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
