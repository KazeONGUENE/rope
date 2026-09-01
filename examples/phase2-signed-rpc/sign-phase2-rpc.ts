// Reference TypeScript client for Phase-2 signed destructive RPC.
//
// Run as a Node 20+ script:
//
//   npm i viem
//   ts-node sign-phase2-rpc.ts 0x<priv-key-hex> [rpc-url] [chain-id]
//
// or copy the helpers into your own SDK. They have no dependency on
// rope-node; they re-implement the canonical-message construction so the
// example is self-contained for partner integrations.
//
// Chain scoping (Phase 0, 2026-08-30):
//   The domain-separation tag is derived from `chainId`. Mainnet (271828)
//   keeps the fixed legacy tag for backward compatibility with every
//   Phase-2 client already in production (DCSwap `quipuEmitter.ts` etc.).
//   Every other chain gets a tag of the shape
//   `DCROPE/destructive-rpc/v1/{chainId}\0`, so a signature minted for
//   testnet (271829) cannot be replayed against mainnet and vice versa.
//   Byte-for-byte parity with rope-node is enforced by
//   `crates/rope-node/src/rpc_signature.rs::chain_domain_tag`.
//
// See `docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md` for the spec.

import {
  privateKeyToAccount,
  type PrivateKeyAccount,
} from "viem/accounts";
import {
  bytesToHex,
  hexToBytes,
  type Hex,
} from "viem";

const MAINNET_CHAIN_ID = 271828n;
const NONCE_LEN = 16;

/** Build the domain-separation tag for `chainId`.
 *
 *  * Mainnet (271828) returns the fixed legacy bytes
 *    `DCROPE/destructive-rpc/v1\0` verbatim.
 *  * Every other chain returns `DCROPE/destructive-rpc/v1/{chainId}\0`
 *    encoded as UTF-8 with a trailing NUL byte.
 *
 *  Must byte-for-byte match
 *  `crates/rope-node/src/rpc_signature.rs::chain_domain_tag`. */
export function chainDomainTag(chainId: bigint): Uint8Array {
  const enc = new TextEncoder();
  if (chainId === MAINNET_CHAIN_ID) {
    return enc.encode("DCROPE/destructive-rpc/v1\0");
  }
  return enc.encode(`DCROPE/destructive-rpc/v1/${chainId.toString()}\0`);
}

function u32be(n: number): Uint8Array {
  const b = new Uint8Array(4);
  new DataView(b.buffer).setUint32(0, n, false);
  return b;
}

function u64be(n: bigint): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, n, false);
  return b;
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const len = parts.reduce((a, p) => a + p.length, 0);
  const out = new Uint8Array(len);
  let i = 0;
  for (const p of parts) {
    out.set(p, i);
    i += p.length;
  }
  return out;
}

/** Build the canonical message bytes the rope-node verifier hashes. Must
 *  byte-for-byte match
 *  `crates/rope-node/src/rpc_signature.rs::canonical_message_with_chain`. */
export function canonicalMessage(
  chainId: bigint,
  method: string,
  paramsWithoutAuth: unknown,
  signedAt: bigint,
  nonce: Uint8Array,
): Uint8Array {
  if (nonce.length !== NONCE_LEN) {
    throw new Error(`nonce must be ${NONCE_LEN} bytes`);
  }
  const methodBytes = new TextEncoder().encode(method);
  const paramsJson = new TextEncoder().encode(JSON.stringify(paramsWithoutAuth));
  return concat(
    chainDomainTag(chainId),
    u32be(methodBytes.length),
    methodBytes,
    u32be(paramsJson.length),
    paramsJson,
    u64be(signedAt),
    nonce,
  );
}

/** Sign canonical bytes per EIP-191 (`personal_sign`) and pack to 65-byte
 *  `r || s || v` form (v ∈ {27, 28}). */
export async function signEip191(
  account: PrivateKeyAccount,
  canonical: Uint8Array,
): Promise<Uint8Array> {
  const sig: Hex = await account.signMessage({ message: { raw: canonical } });
  const bytes = hexToBytes(sig);
  if (bytes.length !== 65) {
    throw new Error(`unexpected signature length: ${bytes.length}`);
  }
  return bytes;
}

/** Build the auth envelope embedded as the LAST element of `params`. */
export function buildAuth(params: {
  signedAt: bigint;
  nonce: Uint8Array;
  signature: Uint8Array;
}): Record<string, unknown> {
  return {
    auth: {
      scheme: "secp256k1-eip191",
      signed_at: Number(params.signedAt), // < 2**53; safe for Unix seconds
      nonce: bytesToHex(params.nonce),
      signature: bytesToHex(params.signature),
    },
  };
}

async function main() {
  const pk = process.argv[2];
  const rpcUrl = process.argv[3] ?? "https://erpc.datachain.network";
  // Fourth positional arg overrides the chain id. Defaults to mainnet
  // (271828) so existing invocations keep working. Set to 271829 (or any
  // other chain) to sign under a chain-scoped tag.
  const chainId = process.argv[4] ? BigInt(process.argv[4]) : MAINNET_CHAIN_ID;
  if (!pk) {
    console.error(
      "usage: ts-node sign-phase2-rpc.ts <0x-priv-key> [rpc-url] [chain-id]",
    );
    process.exit(1);
  }
  const account = privateKeyToAccount(pk as Hex);
  console.log(`signer address: ${account.address}`);
  console.log(`chain id      : ${chainId}`);
  const tag = chainDomainTag(chainId);
  // Trim trailing NUL for pretty-printing only.
  const printableTag =
    tag[tag.length - 1] === 0
      ? new TextDecoder().decode(tag.subarray(0, tag.length - 1))
      : new TextDecoder().decode(tag);
  console.log(`domain tag    : ${printableTag}`);

  const now = BigInt(Math.floor(Date.now() / 1000));
  const nonce = crypto.getRandomValues(new Uint8Array(NONCE_LEN));

  const method = "rope_appendToLedger";
  const paramsWithoutAuth: unknown[] = [
    account.address,
    {
      interaction_type: "TestimonyAttestation",
      description: "phase-2 reference TS client",
      metadata: { client: "examples/phase2-signed-rpc" },
    },
  ];

  const canonical = canonicalMessage(
    chainId,
    method,
    paramsWithoutAuth,
    now,
    nonce,
  );
  const sig = await signEip191(account, canonical);
  const auth = buildAuth({ signedAt: now, nonce, signature: sig });

  const params = [...paramsWithoutAuth, auth];

  const body = {
    jsonrpc: "2.0",
    method,
    params,
    id: 1,
  };

  console.log(`submitting to ${rpcUrl} ...`);
  const resp = await fetch(rpcUrl, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  console.log(`response: ${await resp.text()}`);
}

if (import.meta.url === `file://${process.argv[1]}`) {
  main().catch((e) => {
    console.error(e);
    process.exit(1);
  });
}
