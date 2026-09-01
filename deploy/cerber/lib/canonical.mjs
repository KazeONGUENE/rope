/**
 * Canonical JSON + domain-separated message bytes for CERBER mesh signatures.
 * Deterministic across Node hosts (sorted object keys, no whitespace).
 */

export const DOMAIN_TAG = Buffer.from("DCROPE/cerber-mesh/v1\0", "utf8");

export function canonicalize(value) {
  return JSON.stringify(sortValue(value));
}

function sortValue(v) {
  if (v === null || typeof v !== "object") return v;
  if (Array.isArray(v)) return v.map(sortValue);
  const out = {};
  for (const k of Object.keys(v).sort()) {
    out[k] = sortValue(v[k]);
  }
  return out;
}

/**
 * Build signing input:
 *   DOMAIN || u32be(len(kind)) || kind || u32be(len(body)) || body || u64be(signed_at) || nonce16
 */
export function buildSignInput({ kind, body, signedAt, nonce }) {
  if (typeof kind !== "string" || !kind) throw new Error("kind required");
  if (!Number.isInteger(signedAt) || signedAt <= 0) throw new Error("signedAt required");
  const nonceBuf = Buffer.isBuffer(nonce) ? nonce : Buffer.from(String(nonce).replace(/^0x/, ""), "hex");
  if (nonceBuf.length !== 16) throw new Error("nonce must be 16 bytes");
  const kindBuf = Buffer.from(kind, "utf8");
  const bodyBuf = Buffer.from(typeof body === "string" ? body : canonicalize(body), "utf8");
  const out = Buffer.alloc(
    DOMAIN_TAG.length + 4 + kindBuf.length + 4 + bodyBuf.length + 8 + 16
  );
  let o = 0;
  DOMAIN_TAG.copy(out, o);
  o += DOMAIN_TAG.length;
  out.writeUInt32BE(kindBuf.length, o);
  o += 4;
  kindBuf.copy(out, o);
  o += kindBuf.length;
  out.writeUInt32BE(bodyBuf.length, o);
  o += 4;
  bodyBuf.copy(out, o);
  o += bodyBuf.length;
  out.writeBigUInt64BE(BigInt(signedAt), o);
  o += 8;
  nonceBuf.copy(out, o);
  return out;
}
