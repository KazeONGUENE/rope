import { generateKeyPairSync, createPrivateKey, createPublicKey } from "node:crypto";
import { readFileSync, writeFileSync, mkdirSync, existsSync, chmodSync } from "node:fs";
import { dirname } from "node:path";
import { createHash } from "node:crypto";

/**
 * Ed25519 node identity for CERBER mesh participants.
 * Private key: PKCS8 PEM, mode 0600. Public key: SPKI PEM + raw 32-byte hex kid.
 */

export function ensureIdentity(keyPath, { peerId } = {}) {
  mkdirSync(dirname(keyPath), { recursive: true, mode: 0o700 });
  if (!existsSync(keyPath)) {
    const { privateKey, publicKey } = generateKeyPairSync("ed25519");
    writeFileSync(keyPath, privateKey.export({ type: "pkcs8", format: "pem" }), { mode: 0o600 });
    chmodSync(keyPath, 0o600);
    const pubPath = keyPath.replace(/\.pem$/i, "") + ".pub.pem";
    writeFileSync(pubPath, publicKey.export({ type: "spki", format: "pem" }), { mode: 0o644 });
  }
  return loadIdentity(keyPath, { peerId });
}

export function loadIdentity(keyPath, { peerId } = {}) {
  const pem = readFileSync(keyPath, "utf8");
  const privateKey = createPrivateKey(pem);
  const publicKey = createPublicKey(privateKey);
  const pubDer = publicKey.export({ type: "spki", format: "der" });
  // SPKI for Ed25519 is 12-byte header + 32-byte raw key
  const rawPub = Buffer.from(pubDer).subarray(pubDer.length - 32);
  const kid = createHash("sha256").update(rawPub).digest("hex").slice(0, 16);
  const id = peerId || process.env.CERBER_PEER_ID || `cerber-${kid}`;
  return {
    id,
    kid,
    keyPath,
    privateKey,
    publicKey,
    publicKeyPem: publicKey.export({ type: "spki", format: "pem" }),
    publicKeyHex: rawPub.toString("hex"),
  };
}

export function publicKeyFromPem(pem) {
  return createPublicKey(pem);
}

export function publicKeyFromHex(hex) {
  const raw = Buffer.from(hex.replace(/^0x/, ""), "hex");
  if (raw.length !== 32) throw new Error("ed25519 public key must be 32 bytes");
  // SPKI prefix for Ed25519 OID
  const spkiPrefix = Buffer.from("302a300506032b6570032100", "hex");
  return createPublicKey({ key: Buffer.concat([spkiPrefix, raw]), format: "der", type: "spki" });
}
