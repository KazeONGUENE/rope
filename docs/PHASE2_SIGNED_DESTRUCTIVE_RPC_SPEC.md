# Phase-2 — Signed Destructive-RPC Spec

**Author:** Datachain Rope agent — `/Users/kazealphonseonguene/Downloads/DATACHAIN ROPE/`
**Date drafted:** 2026-06-13
**Last updated:** 2026-08-30 (Phase 0 — chain-scoped `DOMAIN_TAG` for testnet parity, mainnet carve-out preserves wire format)
**Supersedes:** none (extends the V11 hot-fix in `crates/rope-node/src/rpc_auth.rs`)
**Tracking:** ticket #1 in `.cursor/rules/handover-security-audit-2026-06-11.mdc`
**Status:** SPEC (no code yet at draft time); Phase 0 chain-scoping LANDED 2026-08-30 in `rpc_signature.rs` behind `verify_destructive_call_for_chain`, byte-for-byte parity for mainnet enforced by unit tests.

---

## 0. TL;DR

Today, the V11 hot-fix in `rpc_auth.rs` blocks every public caller from invoking the 5 destructive RPC methods on Datachain Rope:

```
rope_untieKnot              -> -32401
rope_erasePersonalLedger    -> -32401
rope_appendToLedger         -> -32401
rope_createPersonalLedger   -> -32401
rope_anchorDeployerAttestation -> -32401
```

Local agents (oracle / insurance / validation / semantic / compliance) bypass the gate by being on `127.0.0.1` without an `X-Forwarded-For` header. That is sound, but it is **operator-grade** authentication, not **caller-grade**. Anyone who runs an agent on a node automatically inherits the full destructive surface; the 5 agent wallets cannot anchor on each other's strings, but a buggy agent can corrupt its own ledger.

Phase-2 replaces the env-flag gate with a **signed-payload requirement** that survives every layer of the stack:

- **Wallet-owned methods** (`rope_untieKnot`, `rope_erasePersonalLedger`, `rope_appendToLedger`, `rope_createPersonalLedger`) — the wallet's secp256k1 EOA key signs the canonical request bytes. The verifier recovers the signing address (Ethereum-style ecrecover) and asserts it equals the wallet in `params[0]`. **Same key the wallet uses everywhere else in the ecosystem.**
- **Governance-owned method** (`rope_anchorDeployerAttestation`) — the founder Ed25519 key signs the canonical request bytes. The verifier loads the founder pubkey from `master-nodes.toml [founder] founder_keys` and checks the signature directly.
- **Replay protection** — every signed request carries a `nonce` (16 random bytes hex-encoded) and a `signed_at` (Unix seconds). Both go through the same `seen_nonces` ring buffer + `±window_secs` time check that `governance.rs` already uses.
- **Loopback bypass remains** — co-located canonical agents on `127.0.0.1` keep the today path, no SDK migration required for them.

Phase-2 ships behind a feature flag (`ROPE_PHASE2_SIGNED_DESTRUCTIVE=1`) so it can roll out to all 4 nodes with the gate INACTIVE, verified for parity, then activated network-wide in a separate flag flip.

---

## 1. Motivation and threat model

### 1.1 What V11 (Phase-1) closed

| Threat | Phase-1 outcome |
|---|---|
| Anonymous public destructive call | ✅ blocked (`-32401` on any non-loopback caller) |
| Proxied public call (`X-Forwarded-For` set) | ✅ blocked |
| Header-injection bypass (`X-Rope-Internal-Token` forged) | ✅ blocked at nginx (header stripped before upstream) |
| Local agent can write to ITS OWN ledger | ✅ allowed (loopback, no XFF) |

### 1.2 What V11 did NOT close

| Threat | Phase-1 outcome |
|---|---|
| Agent A writes garbage to agent B's ledger | ❌ allowed (both are loopback callers) |
| Future remote operator wants to anchor a deployer attestation | ❌ blocked (no caller-grade auth path) |
| Compromised agent process can erase any ledger on the same box | ❌ allowed |
| Off-box wallet owner wants to untie one of their own knots | ❌ blocked (no caller-grade auth path) |

These are exactly the classes of attack a signed-payload requirement closes. The Phase-1 gate trades one risk class (anonymous public destructive) for another (any-local-agent destructive); Phase-2 closes both.

### 1.3 What is OUT of scope for Phase-2

- **DoS-resistance for the destructive surface.** A flood of malformed signed requests still costs the verifier a keccak256 + ecrecover per probe. Rate-limiting and a Phase-3 PoW or paid-call requirement are separate tickets.
- **Hardware-wallet integration.** Phase-2 specifies the wire format and verifier; how the SDK obtains the signature (software keystore, hardware wallet, MetaMask, KMS) is up to the caller.
- **OES-key-shred recovery.** Tombstones and granular erasure remain governed by the existing personal-ledger primitives.
- **Multi-sig for `rope_erasePersonalLedger`.** Whole-wallet erasure could reasonably require 2-of-N signatures rather than 1. Tracked separately.

---

## 2. The five methods, their authority models, and their invariants

| Method | Signer | Invariant the signature MUST prove | Wire shape |
|---|---|---|---|
| `rope_createPersonalLedger` | wallet EOA (params[0]) | "I, the holder of the secp256k1 key whose address is `params[0]`, request that a personal ledger be created at this address." | secp256k1 ECDSA-recover, recovered = lowercase(params[0]) |
| `rope_appendToLedger` | wallet EOA (params[0]) | "I, the holder of params[0], append this exact knot payload to my ledger now." | secp256k1 ECDSA-recover, recovered = params[0]; canonical bytes include `params[1]` (the interaction) |
| `rope_untieKnot` | wallet EOA (params[0]) | "I, the holder of params[0], untie the knot whose string_id is params[1] with reason params[2]." | secp256k1 ECDSA-recover; canonical bytes include all three params |
| `rope_erasePersonalLedger` | wallet EOA (params[0]) | "I, the holder of params[0], erase my entire personal ledger." | secp256k1 ECDSA-recover; canonical bytes include params[0] |
| `rope_anchorDeployerAttestation` | founder Ed25519 (registry) | "I, holder of one of the founder Ed25519 keys in `master-nodes.toml`, instruct this node to anchor its `[deployer]` attestation." | Ed25519 verify against `founder_keys`; canonical bytes include the optional `force` flag |

### 2.1 Why secp256k1 for wallet methods

Datachain Rope wallets are EVM EOAs (20-byte addresses derived from a secp256k1 keypair). Every wallet that has ever touched DCSwap, signed an approval, deployed a contract, or interacted with Tanastok already owns a secp256k1 key. Any signature scheme that does NOT use that key forces wallet owners to maintain a second keypair just for destructive RPC calls — operationally hostile and a blueprint for key-rot regret. Picking secp256k1 + ecrecover means:

- MetaMask `eth_signTypedData_v4` / `personal_sign` produces compatible signatures out of the box.
- ethers.js / viem / wagmi / web3.py / web3.swift / ethers-rs all sign and verify natively.
- Hardware wallets (Ledger, Trezor, GridPlus) sign without firmware updates.
- `ecrecover` is one Rust crate (`k256`) with no `unsafe` and no FFI, used by alloy / foundry / reth in production.

The trade-off is that secp256k1 is **not** post-quantum. That trade-off is acceptable here because:
1. The signature only authorizes a narrow API call. It is not stored on the cord.
2. The cord's chain-continuity and granular-erasure properties (Quipu Canon §6.1.1) still rely on BLAKE3 + ML-DSA-65 (Dilithium3) hybrid; those are post-quantum-secure today.
3. The day a quantum adversary can break secp256k1 is the day every Ethereum-compatible wallet is compromised, at which point the trade-off renegotiates everywhere simultaneously.

### 2.2 Why Ed25519 for the governance method

`rope_anchorDeployerAttestation` is a node-operator action that expresses founder authority. It is already adjacent to `governance.rs`, which already uses Ed25519 against `master-nodes.toml [founder] founder_keys = [...]`. Reusing the same key, the same registry, and the same verifier minimizes new attack surface and keeps the operator runbook simple ("here is one founder key file, signed actions go through it").

---

## 3. Wire format

### 3.1 Auth envelope

Every signed destructive call carries an `auth` object as the LAST element of `params`:

```json
{
  "jsonrpc": "2.0",
  "id": 1,
  "method": "rope_appendToLedger",
  "params": [
    "0x000000000000000000000000000000000000C002",
    { "interaction_type": "TestimonyAttestation",
      "description": "FAT/USDC reserve check",
      "metadata": { "fat_price_usd": "0.0323" } },
    {
      "auth": {
        "scheme": "secp256k1-eip191",
        "signed_at": 1781336400,
        "nonce": "0xe93b3c8d6a14f02f5b1a4d7e3c2b9a08",
        "signature": "0x9b...c4...1c"
      }
    }
  ]
}
```

The auth object lives in a wrapper `{"auth": {...}}` so that callers can extend it later (e.g. `rate_limit_proof`, `paid_call_receipt`) without breaking the schema. The verifier extracts `params[N-1].auth` and rejects if the field is missing or malformed.

### 3.2 The `auth` fields

| Field | Type | Meaning |
|---|---|---|
| `scheme` | string | One of `"secp256k1-eip191"`, `"secp256k1-eip712"`, `"ed25519"`. Phase-2 ships `secp256k1-eip191` (universal MetaMask `personal_sign` compat) and `ed25519` (governance only). `eip712` is a Phase-2.1 ergonomic upgrade. |
| `signed_at` | u64 | Unix seconds at which the caller signed. Verifier rejects if `\|now - signed_at\| > replay_window` (default 300s, taken from `master-nodes.toml [replay] window_secs`). |
| `nonce` | string | Hex-encoded 16-byte random value (`"0x"` + 32 hex chars). Per-signer namespace: the verifier stores nonces keyed by `(signer_pubkey, nonce)`. |
| `signature` | string | Hex-encoded signature. For `secp256k1-eip191`: 65 bytes `(r \|\| s \|\| v)` with `v in {27,28,0,1}`. For `ed25519`: 64 bytes. |

### 3.3 Canonical message

The bytes that get signed are constructed as:

```
canonical_message = domain_tag
                 || method_name_len_le u32
                 || method_name_bytes
                 || canonical_json_params_len_le u32
                 || canonical_json_params
                 || signed_at_be_bytes u64
                 || nonce_bytes
```

Where:

- `domain_tag` is chain-scoped as of Phase 0 (2026-08-30):
  - **Mainnet (chainId 271828):** `b"DCROPE/destructive-rpc/v1\0"` (literal ASCII, NUL-terminated). This is the frozen legacy tag. **Do not change these bytes.** Every Phase-2 client already in production (DCSwap `quipuEmitter.ts`, the operator recovery scripts, `sign_phase2_rpc.rs`, `sign-phase2-rpc.ts`) is pinned to this exact byte string on mainnet, and the nonce store keys off it. A byte-level change is a hard fork of the Phase-2 wire format.
  - **Every other chain (testnet 271829, future rollups, staging chains, …):** `format!("DCROPE/destructive-rpc/v1/{chain_id}\0")` encoded as UTF-8 with a trailing NUL byte. Example: testnet emits `b"DCROPE/destructive-rpc/v1/271829\0"`.
  - Rationale: prevents cross-chain replay of signed destructive calls. A signature minted for testnet cannot be verified against mainnet and vice versa, even when the signer, method, params, `signed_at`, and nonce are byte-identical. The canonical implementation lives in `crates/rope-node/src/rpc_signature.rs::chain_domain_tag` and is exercised by the `chain_domain_tag_*` unit tests; SDK examples MUST match byte-for-byte.
- `method_name_len_le` and `canonical_json_params_len_le` are 4-byte little-endian unsigned integers. They prevent ambiguity attacks where a clever method name could collide with concatenated params.
- `canonical_json_params` is the JSON-canonicalized representation of `params[0..N-1]` (the auth envelope is excluded). Canonicalization rules: object keys sorted lexicographically, no whitespace, UTF-8 NFC, integers in shortest decimal form, no trailing zero in floats. The same `canonical_json_bytes()` function used by `governance.rs::GovernanceAction::canonical_bytes` is reused.
- `signed_at_be_bytes` is the u64 in big-endian (8 bytes). Big-endian for consistency with Ethereum and so the bytes lex-sort the same way as the integer.
- `nonce_bytes` is the 16 raw random bytes (NOT the hex string).

### 3.4 EIP-191 wrapping (secp256k1 schemes)

For `secp256k1-eip191`, the actual digest fed to `ecrecover` is:

```
digest = keccak256( "\x19Ethereum Signed Message:\n" || decimal_len(canonical_message) || canonical_message )
```

This matches MetaMask's `personal_sign` semantics exactly. A wallet owner can sign by calling:

```typescript
const sig = await wallet.signMessage(canonicalMessage)
```

— and the rope-node verifier recovers `(r, s, v)`, computes `digest` the same way, calls `k256::ecdsa::recover_signer(digest, signature)`, and asserts the resulting Ethereum address equals `params[0]`.

### 3.5 Ed25519 (governance scheme)

For `ed25519`, the digest IS the canonical message (Ed25519 is built on SHA-512 internally; the verifier passes `canonical_message` directly to `Verifier::verify`). No EIP-191 wrapper. The signer pubkey is matched against every `founder_keys[]` entry in the registry.

---

## 4. Replay protection

### 4.1 Window

Every signed call is rejected if `|now - signed_at| > window_secs`. Default window is 300 seconds (5 minutes), taken from `master-nodes.toml [replay] window_secs`. Operators can tighten or loosen per-network; tightening reduces the replay surface, loosening accommodates wallets with bad clocks.

### 4.2 Nonce store

The verifier maintains `seen_nonces: DashMap<NonceKey, u64>` where:

```rust
struct NonceKey {
    signer: Vec<u8>, // 20 bytes for secp256k1 address, 32 bytes for ed25519 pubkey
    nonce: [u8; 16],
}
// value is the expires_at Unix-seconds timestamp
```

A background task wakes every 60 seconds and prunes entries with `expires_at < now`. The `expires_at` for each entry is `signed_at + 2 * window_secs` — long enough that no in-flight legitimate call can collide, short enough that the map size stays bounded.

Sizing: at 1000 destructive RPC calls/hour and a 300s window, the steady-state map size is ~166 entries. At 1M calls/hour (catastrophic) it is ~166k entries (~10 MB). Both fit in RAM with no concern. RocksDB-backed persistence is OUT of scope — a node restart wipes the store, and the worst case is a 5-minute window in which the same nonce could replay; given the EOA holds the key and willingly sends the same nonce twice, this is a self-inflicted issue, not an attack.

### 4.3 Per-signer namespace

The nonce key is `(signer, nonce)` rather than just `nonce`. Two reasons:

1. **Privacy.** A flat `nonce` set leaks the global call rate to anyone who can probe the `seen_nonces` size. Per-signer scoping means probing only leaks information about the prober's own signer.
2. **Robustness.** Two different operators can accidentally pick the same 16-byte random value (collision probability is negligible but not zero) without one inadvertently locking out the other.

---

## 5. Verifier integration with the existing Phase-1 gate

The current dispatch logic in `rope_server.rs` (after the V11 hot-fix) is:

```
if !is_internal && rpc_auth::should_deny(method) {
    return -32401
}
```

Phase-2 inserts an additional check between `should_deny` and the deny:

```
if !is_internal && rpc_auth::DESTRUCTIVE_METHODS.contains(method) {
    match rpc_signature::verify_destructive_call(method, params, registry, nonces, now) {
        Ok(VerifiedAuth::WalletEoa(addr)) if addr == params[0] => { /* proceed */ }
        Ok(VerifiedAuth::Founder)                              => { /* proceed */ }
        Ok(VerifiedAuth::WalletEoa(addr))                      => return -32401 ("signer != params[0]")
        Err(e)                                                  => return -32401 (e.detail)
    }
} else if !is_internal && rpc_auth::should_deny(method) {
    return -32401
}
```

Three paths through the gate:

1. **Internal (loopback, no XFF) caller** — same as today. Proceed without signature verification. Canonical agents are unaffected.
2. **External caller with valid signature** — proceed. The signature is validated, the nonce is recorded, and the call is dispatched.
3. **External caller without signature** — `-32401` as today. No regression for blunt-attack-blocking.

The feature flag `ROPE_PHASE2_SIGNED_DESTRUCTIVE` controls path 2:

| `ROPE_PUBLIC_DESTRUCTIVE_DENY` | `ROPE_PHASE2_SIGNED_DESTRUCTIVE` | Behaviour |
|:---:|:---:|---|
| `1` (default) | `0` (default) | Phase-1 only. External callers blocked unconditionally. Today's state. |
| `1` | `1` | Phase-1 + Phase-2. External callers can sign and proceed. **Production target.** |
| `0` | `1` | Phase-2 only. Public callers without signatures pass through. **Discouraged.** Useful for staging-environment testing where the gate is fully off. |
| `0` | `0` | No gate. Pre-V11 state. **Banned in production.** |

Phase-2 ships with `ROPE_PHASE2_SIGNED_DESTRUCTIVE=0` everywhere first, so the verifier code is exercised in production via integration tests but no caller is yet expected to sign. After SDKs are updated and external callers have migrated, a single env-var flip on all 4 nodes activates the path.

---

## 6. Code shape

### 6.1 New module: `crates/rope-node/src/rpc_signature.rs`

```rust
pub enum VerifiedAuth {
    WalletEoa(Address),       // 20 bytes, lowercase hex when stringified
    Founder { pubkey: Vec<u8> },
}

pub enum AuthError {
    MissingEnvelope,
    UnknownScheme(String),
    BadHex(String),
    BadSignatureLength,
    StaleSignature { delta_secs: i64 },
    NonceReplay,
    Recover(String),
    SignerNotAuthority,
    BadCanonicalEncoding(String),
}

pub struct AuthVerifier {
    nonces: dashmap::DashMap<NonceKey, u64>, // expires_at
    window_secs: i64,
    governance: Arc<GovernanceManager>, // re-uses founder_keys from master-nodes.toml
}

impl AuthVerifier {
    pub fn verify_destructive_call(
        &self,
        method: &str,
        params: &serde_json::Value,
        now_unix: i64,
    ) -> Result<VerifiedAuth, AuthError>;

    pub fn prune_nonces(&self, now_unix: i64); // called by background task
}
```

Helpers:

```rust
// Chain-scoped domain tag. Mainnet returns the frozen legacy bytes;
// every other chain returns `DCROPE/destructive-rpc/v1/{chain_id}\0`.
pub fn chain_domain_tag(chain_id: u64) -> Vec<u8>;

// Chain-scoped canonical pre-image. `canonical_message` is a thin wrapper
// that pins `chain_id = MAINNET_CHAIN_ID` (271828) so every mainnet caller
// keeps the exact byte format they had before Phase 0.
pub fn canonical_message(method: &str, params_minus_auth: &serde_json::Value, signed_at: u64, nonce: &[u8; 16]) -> Vec<u8>;
pub fn canonical_message_with_chain(chain_id: u64, method: &str, params_minus_auth: &serde_json::Value, signed_at: u64, nonce: &[u8; 16]) -> Vec<u8>;

// Verifier entry points. `verify_destructive_call` is a mainnet-pinned
// convenience wrapper; nodes on other chains (testnet 271829, etc.) MUST
// call the `_for_chain` variant with their `NodeConfig::node.chain_id`.
pub fn verify_destructive_call(verifier: &AuthVerifier, method: &str, params: &serde_json::Value) -> Result<VerifiedAuth, AuthError>;
pub fn verify_destructive_call_for_chain(verifier: &AuthVerifier, chain_id: u64, method: &str, params: &serde_json::Value) -> Result<VerifiedAuth, AuthError>;

fn extract_auth_envelope(params: &serde_json::Value) -> Result<(AuthEnvelope, serde_json::Value), AuthError>;
fn recover_eip191(canonical_message: &[u8], sig65: &[u8; 65]) -> Result<Address, AuthError>;
fn verify_ed25519(canonical_message: &[u8], sig64: &[u8; 64], pk32: &[u8; 32]) -> Result<(), AuthError>;
```

### 6.2 Cargo dependencies (workspace)

Add to `[workspace.dependencies]` in the root `Cargo.toml`:

```toml
k256 = { version = "0.13", features = ["ecdsa", "sha2", "arithmetic", "expose-field"] }
sha3 = "0.10"  # for keccak256
```

In `crates/rope-node/Cargo.toml`:

```toml
[dependencies]
k256.workspace = true
sha3.workspace = true
# ed25519-dalek already present via governance.rs
```

`k256` is the RustCrypto pure-Rust secp256k1 implementation. It is the same crate used by alloy and ethers-rs, audited, no `unsafe`. `sha3` provides keccak256.

### 6.3 Wire change in `rope_server.rs::handle_json_rpc_with_auth`

Two-line patch to the destructive-method dispatch:

```rust
let auth_outcome = if is_internal {
    AuthOutcome::Loopback
} else if !rpc_auth::DESTRUCTIVE_METHODS.contains(&method) {
    AuthOutcome::NotApplicable
} else if !std::env::var("ROPE_PHASE2_SIGNED_DESTRUCTIVE").map(|v| v == "1").unwrap_or(false) {
    // Phase-2 not yet active. Fall back to Phase-1 deny.
    return rpc_auth::denied_response(&id);
} else {
    match self.auth_verifier.verify_destructive_call(method, params, chrono::Utc::now().timestamp()) {
        Ok(VerifiedAuth::WalletEoa(addr)) if matches_param0(addr, params) => AuthOutcome::SignedWallet(addr),
        Ok(VerifiedAuth::Founder { .. }) if method == "rope_anchorDeployerAttestation" => AuthOutcome::SignedFounder,
        Ok(_) | Err(_) => return rpc_auth::denied_response(&id),
    }
};
```

The existing dispatch arms then read `auth_outcome` to populate the response's `auth_method` field. (The 5 destructive methods already include this field; for Phase-2 calls it transitions from `"phase-1-trusted-proxy"` to `"phase-2-eip191"` or `"phase-2-ed25519-founder"`.)

---

## 7. SDK migration paths

### 7.1 TypeScript / viem

```typescript
import { keccak256, hexToBytes, toHex, encodePacked } from "viem";
import { privateKeyToAccount } from "viem/accounts";

// Chain scoping (Phase 0, 2026-08-30):
//   mainnet (271828)  -> b"DCROPE/destructive-rpc/v1\0"  (legacy, frozen)
//   any other chainId -> `DCROPE/destructive-rpc/v1/{chainId}\0`
// Byte-for-byte parity with rope-node is enforced by
// `crates/rope-node/src/rpc_signature.rs::chain_domain_tag`.
function chainDomainTag(chainId: bigint): Uint8Array {
  const enc = new TextEncoder();
  if (chainId === 271828n) return enc.encode("DCROPE/destructive-rpc/v1\0");
  return enc.encode(`DCROPE/destructive-rpc/v1/${chainId.toString()}\0`);
}

async function callDestructiveRpc(
  rpcUrl: string,
  chainId: bigint,
  method: string,
  paramsMinusAuth: any[],
  pk: `0x${string}`,
) {
  const account = privateKeyToAccount(pk);
  const signedAt = Math.floor(Date.now() / 1000);
  const nonce = crypto.getRandomValues(new Uint8Array(16));

  const domainTag = chainDomainTag(chainId);
  const methodBytes = new TextEncoder().encode(method);
  const canonicalParams = canonicalJsonBytes(paramsMinusAuth);

  const lenLE = (n: number) => { const b = new Uint8Array(4); new DataView(b.buffer).setUint32(0, n, true); return b; };
  const u64BE = (n: number) => {
    const b = new Uint8Array(8);
    const dv = new DataView(b.buffer);
    dv.setBigUint64(0, BigInt(n), false);
    return b;
  };

  const message = concat([domainTag, lenLE(methodBytes.length), methodBytes, lenLE(canonicalParams.length), canonicalParams, u64BE(signedAt), nonce]);
  const signature = await account.signMessage({ message: { raw: message } });

  return await fetch(rpcUrl, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0", id: 1, method,
      params: [...paramsMinusAuth, { auth: { scheme: "secp256k1-eip191", signed_at: signedAt, nonce: toHex(nonce), signature } }],
    }),
  }).then(r => r.json());
}
```

A reference implementation lives at `crates/rope-cli/sdk/typescript/destructive-rpc.ts` once Phase-2 ships.

### 7.2 Rust (off-box callers)

```rust
use k256::ecdsa::{SigningKey, Signature, signature::Signer};
use sha3::{Digest, Keccak256};

// Chain scoping (Phase 0, 2026-08-30):
//   mainnet (271828)  -> b"DCROPE/destructive-rpc/v1\0"  (legacy, frozen)
//   any other chainId -> `DCROPE/destructive-rpc/v1/{chain_id}\0`
// Match `crates/rope-node/src/rpc_signature.rs::chain_domain_tag` byte-for-byte.
fn chain_domain_tag(chain_id: u64) -> Vec<u8> {
    const MAINNET: u64 = 271828;
    if chain_id == MAINNET {
        b"DCROPE/destructive-rpc/v1\0".to_vec()
    } else {
        let mut tag = format!("DCROPE/destructive-rpc/v1/{chain_id}").into_bytes();
        tag.push(0);
        tag
    }
}

fn sign_destructive_call(chain_id: u64, method: &str, params_minus_auth: &serde_json::Value, sk: &SigningKey) -> AuthEnvelope {
    let signed_at = chrono::Utc::now().timestamp() as u64;
    let mut nonce = [0u8; 16];
    rand::thread_rng().fill(&mut nonce);

    let canonical = canonical_message(chain_id, method, params_minus_auth, signed_at, &nonce);
    // EIP-191 wrap
    let mut hasher = Keccak256::new();
    hasher.update(b"\x19Ethereum Signed Message:\n");
    hasher.update(canonical.len().to_string().as_bytes());
    hasher.update(&canonical);
    let digest = hasher.finalize();

    let (signature, recovery_id) = sk.sign_prehash_recoverable(&digest).unwrap();
    let mut sig65 = [0u8; 65];
    sig65[..64].copy_from_slice(&signature.to_bytes());
    sig65[64] = recovery_id.to_byte() + 27;

    AuthEnvelope {
        scheme: "secp256k1-eip191".into(),
        signed_at,
        nonce: format!("0x{}", hex::encode(nonce)),
        signature: format!("0x{}", hex::encode(sig65)),
    }
}
```

Reference helper: `crates/rope-cli/src/sign_rpc.rs` (new in Phase-2).

### 7.3 The 5 canonical agents

**Zero changes.** They keep using `http://127.0.0.1:8545` without `X-Forwarded-For`, hit the loopback bypass, and the gate is never invoked.

---

## 8. Test plan

### 8.1 Unit tests in `rpc_signature.rs`

| Test | Asserts |
|---|---|
| `canonical_message_is_deterministic` | Two calls with the same inputs produce byte-identical output |
| `canonical_message_distinguishes_methods` | Same params with different methods produce different messages |
| `canonical_message_distinguishes_signed_at` | Same params with `signed_at` differing by 1 produce different messages |
| `canonical_message_distinguishes_nonce` | Same params with different nonces produce different messages |
| `eip191_wrapper_matches_metamask` | Hard-coded MetaMask test vector recovers the expected address |
| `recover_eip191_recovers_correct_address` | Sign with a known SK, recover, equals `address_from_pubkey(sk.verifying_key())` |
| `recover_eip191_rejects_wrong_message` | Sign over message A, verify against message B, fails |
| `verify_ed25519_accepts_known_test_vector` | RFC 8032 Ed25519 test vector passes |
| `nonce_replay_rejected` | Same `(signer, nonce)` twice -> `NonceReplay` |
| `stale_signature_rejected_above_window` | `signed_at = now - 600` with `window = 300` -> `StaleSignature` |
| `future_signature_rejected_above_window` | `signed_at = now + 600` -> `StaleSignature` |
| `unknown_scheme_rejected` | `scheme = "ml-dsa-65"` -> `UnknownScheme` |
| `bad_signature_length_rejected` | 32-byte signature for secp256k1 -> `BadSignatureLength` |
| `bad_hex_rejected` | `nonce = "not-hex"` -> `BadHex` |
| `signer_not_in_founder_set_rejected` | Ed25519 signer not in `founder_keys` -> `SignerNotAuthority` |
| `phase2_flag_off_blocks_external_caller` | `ROPE_PHASE2_SIGNED_DESTRUCTIVE != 1` and external caller -> -32401 (Phase-1 fallback) |
| `chain_domain_tag_mainnet_is_legacy_bytes` | `chain_domain_tag(271828) == b"DCROPE/destructive-rpc/v1\0"` byte-for-byte |
| `chain_domain_tag_testnet_embeds_chain_id` | `chain_domain_tag(271829) == b"DCROPE/destructive-rpc/v1/271829\0"` |
| `chain_domain_tag_arbitrary_chain_embeds_chain_id` | `chain_domain_tag(42) == b"DCROPE/destructive-rpc/v1/42\0"` |
| `canonical_message_wrapper_pins_mainnet` | `canonical_message(...)` output byte-equals `canonical_message_with_chain(271828, ...)` output for the same inputs |
| `canonical_message_distinguishes_chain_ids` | Same method / params / signed_at / nonce with `chain_id = 271828` vs `271829` produces different bytes |
| `cross_chain_replay_rejected` | Signature minted for testnet (271829) verified against mainnet verifier (`verify_destructive_call`) -> `SignerMismatch` / recovered address mismatch |
| `cross_chain_replay_mainnet_to_testnet_rejected` | Signature minted for mainnet verified via `verify_destructive_call_for_chain(_, 271829, ..)` -> address mismatch |

### 8.2 Integration tests in `rpc_server.rs`

| Test | Asserts |
|---|---|
| `loopback_bypass_still_works_phase2_on` | With Phase-2 ON, loopback caller (no XFF) skips signature verification, proceeds |
| `external_signed_append_succeeds` | With Phase-2 ON, external caller signs, response succeeds, ledger contains the appended interaction, `auth_method = "phase-2-eip191"` |
| `external_signed_with_wrong_address_rejected` | Sign with key X, declare params[0] = address Y -> -32401 |
| `external_signed_replays_rejected` | Same signed payload sent twice -> first succeeds, second -> -32401 |
| `external_unsigned_still_blocked` | With Phase-2 ON, external caller without auth envelope -> -32401 |
| `governance_method_only_accepts_founder_ed25519` | `rope_anchorDeployerAttestation` signed by a non-founder Ed25519 key -> -32401 |
| `governance_method_accepts_founder_ed25519` | `rope_anchorDeployerAttestation` signed by a key in `founder_keys` -> 200 |
| `feature_flag_off_uses_phase1` | With `ROPE_PHASE2_SIGNED_DESTRUCTIVE=0`, signed external call -> -32401 (Phase-1 deny still applies) |
| `nonce_pruning_actually_prunes` | Insert `signed_at = now - 700` (2x window), call `prune_nonces(now)`, map size shrinks |

### 8.3 End-to-end smoke tests (added to `deploy-fleet.sh --smoke-test`)

| Probe | Expected |
|---|---|
| `rope_appendToLedger` with valid signature against `0xC003` (insurance-agent wallet) | 200 OK, response includes `auth_method = "phase-2-eip191"` |
| Same probe replayed | `-32401` with detail "nonce replay" |
| Same probe with `signed_at` offset by 600s | `-32401` with detail "stale signature" |
| `rope_anchorDeployerAttestation` signed by founder | 200 OK, response includes `auth_method = "phase-2-ed25519-founder"` |
| `rope_anchorDeployerAttestation` signed by random Ed25519 key | `-32401` |

---

## 9. Roll-out plan

The roll-out is purposely incremental. Every step is reversible, and all four nodes stay in sync via `deploy-fleet.sh`.

### Step 1 — code merge with feature flag OFF (this is what Phase-2 ships first)

- `crates/rope-node/src/rpc_signature.rs` lands with full unit + integration test coverage.
- `handle_json_rpc_with_auth` consults `ROPE_PHASE2_SIGNED_DESTRUCTIVE`. With the flag default-off, behaviour is byte-identical to today's V11 hot-fix.
- `deploy-fleet.sh` deploys the new binary to all 4 nodes (GREEN -> DO1 -> DO2 -> BLUE). Smoke test passes today's V11 audit unchanged.
- A new smoke probe `--phase2-shadow` runs the verifier against a known signed request without enforcing — useful to confirm parity in production traffic before activating.

### Step 2 — sign one canonical-agent ledger creation against staging

- A staging environment with `ROPE_PHASE2_SIGNED_DESTRUCTIVE=1` and a fresh chain ID.
- A scripted Hardhat test signs `rope_createPersonalLedger`, `rope_appendToLedger`, `rope_untieKnot`, `rope_erasePersonalLedger`, `rope_anchorDeployerAttestation` end-to-end.
- All five succeed. Replays fail. Stale-time fails. Wrong-signer fails.

### Step 3 — TypeScript SDK landed in `crates/rope-cli/sdk/typescript/`

- Includes a `signDestructiveCall(method, params, account)` helper.
- Includes a vitest suite that hits a local `rope-node` test instance with the flag on.
- Published as `@datachain/rope-sdk@0.2.0-phase2` once verified.

### Step 4 — external partners migrate (Tanastok, DCSwap, Datawallet+, NaturaProof, Careaway)

- Handover doc with examples in `.cursor/rules/handover-phase2-signed-destructive-rpc-2026-XX-XX.mdc`.
- Each partner migrates their off-box callers, if any. The 5 canonical agents on rope-vps need no change.

### Step 5 — flag flip on production

- `ROPE_PHASE2_SIGNED_DESTRUCTIVE=1` set in `deploy/config/.env` on all 4 nodes.
- `deploy-fleet.sh --restart-services` rolls the change out (a new mode added in Phase-2; just calls `systemctl restart datachain-rope.service` on every node).
- Smoke test now includes the live signed-RPC probes from §8.3.

### Step 6 — observe for 30 days

- Track `auth_method` distribution via dc-explorer logs.
- If "phase-1-trusted-proxy" appears for any non-loopback caller after 30 days, file an incident; that means a partner has not migrated.

### Step 7 — remove `ROPE_PUBLIC_DESTRUCTIVE_DENY=0` opt-out

- The Phase-1 env-flag escape hatch (which still allows operators to disable the gate entirely) is removed in a follow-up PR. The only sanctioned bypass becomes loopback-without-XFF, and the only sanctioned external path becomes a valid signature.

### Rollback at any step

If anything goes wrong at any stage, the rollback is to set `ROPE_PHASE2_SIGNED_DESTRUCTIVE=0` and restart `datachain-rope.service`. Behaviour reverts to today's V11 hot-fix immediately. No code rollback is required.

---

## 10. Open questions to revisit during implementation

| Question | Today's lean | Decision criterion |
|---|---|---|
| Should we accept `secp256k1-eip712` (typed-data signing) as a second scheme? | Defer to Phase-2.1. Wallet support is universal for `personal_sign`; EIP-712 needs per-method type definitions. | If wallet UX during user testing demands typed signing for clarity. |
| Should `rope_erasePersonalLedger` require multi-sig? | Single-sig today, multi-sig in Phase-3. Wallet owners losing their key would lose access to erasure too if multi-sig were required. | If a regulator or DPO objects to single-sig erasure semantics. |
| Should the verifier expose call-rate metrics per signer? | Yes via `tracing::info!(target: "rope_node::auth", signer = ..., method = ...)`. | If observability gaps emerge during Step 6. |
| Should signed calls anchor a meta-knot recording the signer pubkey? | Yes — this is what the existing `auth_method` field hints at. The Phase-2 implementation should add `signer_address` (for ECDSA) or `signer_pubkey` (for Ed25519) to the response. | Always — it costs nothing and is invaluable for forensics. |
| Should we honor `nonce` as a u64 instead of 16 bytes? | 16 bytes; u64 is too small for collision resistance over multi-year operation. | Already settled. |
| Should the Ed25519 path use BLAKE3 instead of plain canonical-message-as-digest? | No — Ed25519 is built on SHA-512 and the standard library handles the prehash. Avoid extra hashing. | Already settled. |

---

## 11. References

- `crates/rope-node/src/rpc_auth.rs` — Phase-1 V11 hot-fix and the `DESTRUCTIVE_METHODS` constant Phase-2 reuses.
- `crates/rope-node/src/governance.rs` — the existing Ed25519 + nonce + replay-window pattern Phase-2 mirrors.
- `crates/rope-node/src/rpc_server.rs` — destructive method handlers (around lines 1805, 1844, 2009, 2090, 3344).
- `deploy/config/master-nodes.toml` — `[founder] founder_keys` registry for Ed25519 path.
- `.cursor/rules/handover-security-audit-2026-06-11.mdc` — the audit report this spec answers.
- `.cursor/rules/quipu-canon-knot-hash-construction.mdc` — adjacent canon work; Phase-2 does NOT touch the knot path.
- `k256` crate — RustCrypto secp256k1, used for ECDSA-recover.
- EIP-191 — `personal_sign` wrapper (`"\x19Ethereum Signed Message:\n" + len + msg`).
- RFC 8032 — Ed25519 specification.

---

*This spec is the authoritative reference for the Phase-2 V11 closure. Every code change tracked under ticket #1 in the audit handover MUST cite this document. Roll-out diverges from §9 only with explicit operator sign-off.*
