import { sign as cryptoSign, verify as cryptoVerify, randomBytes, createHash } from "node:crypto";
import { buildSignInput, canonicalize } from "./canonical.mjs";
import { publicKeyFromHex, publicKeyFromPem } from "./identity.mjs";

const FRESHNESS_SECS = Number(process.env.CERBER_SIG_FRESHNESS_SECS ?? 600);

function bodySha256(body) {
  const raw = typeof body === "string" ? body : canonicalize(body);
  return createHash("sha256").update(raw).digest("hex");
}

/**
 * Sign a CERBER mesh interaction. Returns the detached envelope (body kept separate).
 */
export function signEnvelope(identity, { kind, body, signedAt, nonce } = {}) {
  const at = signedAt ?? Math.floor(Date.now() / 1000);
  const n = nonce ?? randomBytes(16);
  const input = buildSignInput({ kind, body, signedAt: at, nonce: n });
  const sig = cryptoSign(null, input, identity.privateKey);
  return {
    scheme: "ed25519-cerber-mesh-v1",
    peer_id: identity.id,
    kid: identity.kid,
    public_key: identity.publicKeyHex,
    kind,
    signed_at: at,
    nonce: "0x" + Buffer.from(n).toString("hex"),
    signature: "0x" + sig.toString("hex"),
    body_sha256: bodySha256(body),
  };
}

export const signInteraction = signEnvelope;

/**
 * Verify envelope against body.
 * `trustedKeys`: map peer_id|kid|pubkeyhex → pubkeyhex|true, or (peerId, kid, pubHex) => bool.
 */
export function verifyEnvelope(envelope, body, { trustedKeys, now, maxSkewSecs } = {}) {
  const skew = maxSkewSecs ?? FRESHNESS_SECS;
  const t = now ?? Math.floor(Date.now() / 1000);
  if (!envelope || envelope.scheme !== "ed25519-cerber-mesh-v1") {
    return { ok: false, reason: "bad_scheme" };
  }
  if (!Number.isInteger(envelope.signed_at)) return { ok: false, reason: "bad_signed_at" };
  if (Math.abs(t - envelope.signed_at) > skew) return { ok: false, reason: "stale_or_future" };
  if (bodySha256(body) !== envelope.body_sha256) return { ok: false, reason: "body_hash_mismatch" };

  const pubHex = String(envelope.public_key || "").replace(/^0x/, "");
  if (trustedKeys) {
    const allow =
      typeof trustedKeys === "function"
        ? trustedKeys(envelope.peer_id, envelope.kid, pubHex)
        : trustedKeys[envelope.peer_id] === pubHex ||
          trustedKeys[envelope.kid] === pubHex ||
          trustedKeys[pubHex] === true;
    if (!allow) return { ok: false, reason: "untrusted_key" };
  }

  let key;
  try {
    key = publicKeyFromHex(pubHex);
  } catch {
    try {
      if (!envelope.public_key_pem) throw new Error("no pem");
      key = publicKeyFromPem(envelope.public_key_pem);
    } catch (e) {
      return { ok: false, reason: `bad_public_key:${e.message}` };
    }
  }

  const nonce = Buffer.from(String(envelope.nonce).replace(/^0x/, ""), "hex");
  if (nonce.length !== 16) return { ok: false, reason: "bad_nonce" };
  const input = buildSignInput({
    kind: envelope.kind,
    body,
    signedAt: envelope.signed_at,
    nonce,
  });
  const sig = Buffer.from(String(envelope.signature).replace(/^0x/, ""), "hex");
  const ok = cryptoVerify(null, input, key, sig);
  return ok ? { ok: true, peer_id: envelope.peer_id, kid: envelope.kid } : { ok: false, reason: "bad_signature" };
}

export function wrapSigned(identity, kind, body) {
  return { body, envelope: signEnvelope(identity, { kind, body }) };
}
