# Phase-2 Signed Destructive RPC — SDK Examples

Two reference implementations that produce a valid Phase-2 auth envelope and
submit a destructive `rope_*` JSON-RPC call against a Phase-2-enabled rope-node.

| File | Language | Library | What it shows |
|---|---|---|---|
| `sign_phase2_rpc.rs` | Rust | `k256`, `sha3`, `serde_json`, `reqwest` | Build the canonical message, sign with secp256k1 + EIP-191, embed the auth envelope, submit. |
| `sign-phase2-rpc.ts` | TypeScript | `viem` | Same flow with viem's `signMessage` and a minimal `fetch` client. |

Both examples target `rope_appendToLedger` because it is the most common
wallet-owned destructive call. Switching to `rope_untieKnot` /
`rope_erasePersonalLedger` / `rope_createPersonalLedger` is a one-line change
(method name + params shape per `docs/PHASE2_SIGNED_DESTRUCTIVE_RPC_SPEC.md`).

`rope_anchorDeployerAttestation` requires the Ed25519 / founder-key path; that
is documented in the spec but not included here as a runnable example because
the founder key is never to be exported to a user machine.

## Wire shape (recap)

The auth envelope is the LAST element of `params`. The verifier strips it
before the dispatch handlers run, so the existing `params[0]`, `params[1]`,
... indices in rope-node are unchanged.

```json
{
  "jsonrpc": "2.0",
  "method": "rope_appendToLedger",
  "params": [
    "0x...wallet...",
    { "interaction_type": "TestimonyAttestation", "description": "...", "metadata": {} },
    {
      "auth": {
        "scheme": "secp256k1-eip191",
        "signed_at": 1781336400,
        "nonce": "0x<32-hex-chars>",
        "signature": "0x<130-hex-chars>"
      }
    }
  ],
  "id": 1
}
```

## Canonical message bytes

All clients MUST hash the same bytes the verifier hashes. The layout is:

```
DOMAIN_TAG ("DCROPE/destructive-rpc/v1\0", 26 bytes) ||
u32_be(len(method)) || method_bytes ||
u32_be(len(params_without_auth_serialized)) || params_without_auth_serialized ||
u64_be(signed_at) ||
nonce (16 bytes)
```

`params_without_auth_serialized` = `serde_json::to_vec(&params_without_auth)`
in Rust, or the equivalent compact JSON serialization in TypeScript. **Do not
pretty-print and do not reorder keys**; both clients here use the default
"keys-as-inserted" serialization. If two implementations disagree, the simplest
debugging step is to log the canonical-message hex on both sides and diff.

The signed digest for secp256k1 is then EIP-191:

```
keccak256("\x19Ethereum Signed Message:\n" || ascii(len(canonical)) || canonical)
```

This matches `personal_sign` in MetaMask, viem, ethers.js, web3.js, web3.py,
web3.swift — i.e. every mainstream Ethereum SDK already produces a valid
Phase-2 signature when fed the canonical bytes.
