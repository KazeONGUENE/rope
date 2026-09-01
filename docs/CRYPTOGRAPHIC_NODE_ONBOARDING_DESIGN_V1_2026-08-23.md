# Cryptographic Node Onboarding Design v1 (2026-08-23)

**Status:** DESIGN. Not implemented. Grounded in primitives that already exist in `rope-consensus`, `rope-crypto`, and `master-nodes.toml`.
**Author:** Datachain Rope agent
**Trigger:** operator asked "shouldn't we implement an asymmetric-based authentication key enabling the Datachain Rope to automatically recognize new nodes added to the fleet (nodes deployed through accredited cloud providers like Exoscale or Databox etc.)" (2026-08-23)

---

## 0. TL;DR

- **The identity primitives already exist.** `rope-consensus::ValidatorRegistry` binds a `NodeId = blake3(ed25519_pubkey)` to a `HybridPublicKey` (Ed25519 + Dilithium3, PQ-ready) with identity-mismatch checks that prevent a node from claiming an ID it does not hold the key for. `master-nodes.toml` already contains founder Ed25519 keys.
- **What is missing** is (1) the on-chain node registry that lets attesters bootstrap dynamically instead of via hand-edited TOML, (2) a provider-attestation envelope so accredited clouds (Exoscale, Databox, etc.) can sponsor a node without holding a founder key, and (3) a dynamic nginx upstream loader so admission doesn't require a hand-edit + reload.
- **Design shape:** an on-chain `NodeRegistry` string (Quipu Canon v1.2 `kind=node`) holds every admissible node's `HybridPublicKey` and provider attestation. Nodes join via a signed `HELLO` handshake verified against this string. Nginx reads its upstream list from a signed JSON file that a small daemon renders from the on-chain string every 60 s.
- **This is a v1 design.** It ships in three additive phases that never break the current hardcoded-upstream fleet: (Phase A) on-chain registry + read-only fleet-status field; (Phase B) HELLO handshake + rotation/revocation; (Phase C) dynamic nginx upstream loader + third-party provider attestation.

---

## 1. What already exists (the foundation)

### 1.1 `master-nodes.toml`
Location: `deploy/config/master-nodes.toml` on each production node.
Loaded by: `crates/rope-node/src/governance.rs::GovernanceManager::from_file` at startup, plus `crates/rope-explorer/src/admin_tokens.rs::load_founder_keys` at explorer startup.
Contents:
- `master_nodes[]` - the current sealer + attester roster with Ed25519 pubkeys
- `member_nodes[]` - allowlisted follower/edge nodes
- `founder.founder_keys[]` - founder Ed25519 verifying keys (used for admin-token bootstrap + Phase-2 destructive-RPC founder attestations)

Constraint: **hand-edited**. Adding a new node today = SSH into every VPS + edit + restart. No hot-reload path is safe.

### 1.2 `ValidatorRegistry` (Quipu Canon v2.0 Phase 2, CODE-COMPLETE)
Location: `crates/rope-consensus/src/validator_registry.rs`.
Provides:
- `NodeId = blake3(ed25519_pubkey)` (identity is bound to key material, not to IP or hostname)
- `register(node_id, HybridPublicKey)` with `IdentityMismatch` rejection if the supplied id does not equal the derived id
- `MissingPostQuantumKey` rejection if the key has no Dilithium3 component (hybrid PQ mandate)
- `is_active(node_id)` for live-committee lookups
- `ValidatorSetSnapshot` for serializable committee ship-out (config, gossip, or on-chain)

Not yet wired to a bootstrap flow - today the registry is populated at process start from a snapshot, not from a live handshake.

### 1.3 `HybridPublicKey` (Ed25519 + Dilithium3)
Location: `crates/rope-crypto/src/hybrid.rs`.
Provides the actual signing / verification primitives. Already used for testimony verification. Ready to be used for node HELLO too.

### 1.4 Phase-2 signed destructive RPC verifier
Location: `crates/rope-node/src/rpc_signature.rs`.
Provides a proven pattern: `DOMAIN_TAG || canonical_message_bytes || nonce (16 bytes) || u64_be(signed_at)` with a `dashmap`-backed `(signer, nonce)` replay store and ±window_secs freshness. The HELLO handshake below reuses this exact construction.

### 1.5 Quipu Canon v1.2 string registry
Location: `crates/rope-node/src/ledger_manager.rs` + `crates/rope-core/src/personal_ledger.rs`.
Provides: per-entity strings with 5 canonical kinds (`wallet`, `contract`, `asset`, `did`, `cord`). A **6th kind = `node`** is the natural home for the on-chain node registry. Adding a kind is a canon-adjacent change; see §3.3 below.

---

## 2. Threat model

| Threat | Today | With v1 design |
|---|---|---|
| Attacker adds a rogue node to the fleet | Blocked by hardcoded nginx upstream + UFW per-IP allow rules | Blocked by on-chain registry (founder + provider signatures required) |
| Attacker steals a node's key | Full impersonation until human notices + edits `master-nodes.toml` on every VPS | Founder or guardian issues an on-chain revocation knot; nginx auto-drops within 60 s |
| Cloud provider is compromised | Provider's sponsorship weight collapses; nodes it attested to still valid but no new ones admitted | Same, but revocation is auditable on-chain |
| A node lies about its `NodeId` | Blocked by `ValidatorRegistry::register`'s `IdentityMismatch` check | Same; the check runs on every HELLO |
| Replay of a stolen HELLO | Not applicable today | Blocked by `(signer, nonce)` store + ±300 s freshness (same construction as Phase-2 RPC) |
| DoS via HELLO flood | Not applicable today | Rate-limited per source IP at nginx; HELLO handshake is stateless verification (no DB write) until admission |
| Founder key compromise | Catastrophic | Founder key rotation via a 2-of-N founder multisig (Phase-2 destructive RPC pattern already supports this) |

---

## 3. Design

### 3.1 On-chain node registry (Phase A - additive, no behavioural change)

Extend Quipu Canon v1.2 string registry with a 6th kind:

```rust
pub enum StringKind {
    Wallet,
    Contract,
    Asset,
    Did,
    Cord,
    Node, // NEW in v1.2.1
}
```

Node string layout:

- `string_id = keccak256("dcrope://node/" || hex(ed25519_pubkey))` (deterministic; a node cannot create two strings for the same key)
- Genesis knot payload:
  ```json
  {
    "event_type": "NodeRegistered",
    "public_key_ed25519": "<hex>",
    "public_key_dilithium3": "<hex>",
    "role": "sealer_candidate | attester | reader_only",
    "provider_attestation": { ... },
    "operator_email": "ops@example.org",
    "founder_signature": "<hex ed25519 sig over the canonical registration bytes>"
  }
  ```
- Subsequent knots: `NodeRotated`, `NodeRevoked`, `NodePromoted` (reader -> attester -> sealer_candidate), `NodeDemoted`.

The `founder_signature` requirement means Phase A is still founder-gated (same trust surface as `master-nodes.toml` today), just moved from hand-edited TOML to on-chain. This is the safe first step: no new trust roots.

Phase A ships when:
- `StringKind::Node` is added to `personal_ledger.rs` + `ledger_manager.rs` + `rpc_server.rs::rope_listStrings` + `rope-explorer` v1.2 registry API.
- `rope-explorer` renders `/api/v1/nodes` (list) and `/api/v1/nodes/:pubkey` (detail).
- `fleet-status.json` gains a `known_nodes[]` field populated from the on-chain registry.

At Phase A end, everything still runs on hardcoded nginx upstreams. The on-chain registry is *observability only* (dcscan renders it; operators verify the roster matches `master-nodes.toml`).

### 3.2 HELLO handshake (Phase B - additive, opt-in)

New RPC method: `rope_nodeHello(request)` where `request` is:

```json
{
  "public_key_ed25519": "<hex>",
  "public_key_dilithium3": "<hex>",
  "role_claimed": "reader_only",
  "provider_attestation": { ... },
  "listen_addr": "<host>:<port>",
  "signed_at": 1786928234,
  "nonce": "<16 hex bytes>",
  "signature_ed25519": "<hex>",
  "signature_dilithium3": "<hex>"
}
```

Canonical bytes (verifier and node MUST match byte-for-byte):

```
DOMAIN_TAG ("DCROPE/node-hello/v1\0", 22 bytes)
|| u32_be(len(public_key_ed25519)) || public_key_ed25519
|| u32_be(len(public_key_dilithium3)) || public_key_dilithium3
|| u32_be(len(role_claimed)) || role_claimed_utf8
|| u32_be(len(provider_attestation_canonical_json)) || provider_attestation_canonical_json
|| u32_be(len(listen_addr)) || listen_addr_utf8
|| u64_be(signed_at)
|| nonce (16 bytes)
```

Verifier flow (server-side, in `rope-node`):

1. Reject if `signed_at` is > 300 s in the past or future.
2. Reject if `(public_key_ed25519, nonce)` is in the replay store (same `dashmap` as Phase-2 RPC).
3. Reject if `provider_attestation` fails validation (§3.4).
4. Reject if `signature_ed25519` does not verify over canonical bytes with `public_key_ed25519`.
5. Reject if `signature_dilithium3` does not verify over canonical bytes with `public_key_dilithium3`.
6. Insert `(public_key_ed25519, nonce)` into the replay store.
7. Append a `NodeHelloAccepted` knot to the node's registry string (creating the string if genesis has not landed yet).
8. Return `{ accepted: true, node_id: "<blake3(ed25519)>", role_granted: "reader_only" }`.

Note the granted role is initially always `reader_only`. Promotion to `attester` requires a founder-signed `NodePromoted` knot. Promotion to `sealer_candidate` requires additional soak criteria + operator approval.

Phase B ships when:
- `rope_nodeHello` is implemented + tested (target: 20+ unit tests, same coverage bar as `rpc_signature.rs`).
- The HELLO endpoint is exposed on `erpc.datachain.network` under `/v1/nodes/hello` (POST-only, rate-limited, no browser CORS - server-to-server only).
- Node CLI (`rope-cli node hello --provider-attestation=...`) supports the client side.

### 3.3 Provider attestation envelope (Phase B, part of §3.2)

An accredited cloud provider (Exoscale, Databox, ...) publishes a well-known Ed25519 verifying key at `https://<provider>/.well-known/dcrope-provider.json` and signs each node they sponsor:

```json
{
  "provider_id": "exoscale-ch-gva-2",
  "provider_pubkey_ed25519": "<hex>",
  "node_pubkey_ed25519": "<hex>",
  "sponsored_at": 1786928000,
  "sponsorship_expires_at": 1818464000,
  "sponsorship_terms": {
    "sla_tier": "gold",
    "geo_region": "ch-gva",
    "compute_class": "n1-standard-8"
  },
  "signature": "<hex ed25519 sig over the canonical attestation bytes>"
}
```

The verifier trusts a `provider_id` only if it is listed in `deploy/config/providers.toml` (a new file, founder-editable, same trust surface as `master-nodes.toml`). Founder keys sign `providers.toml` too; a provider cannot self-list.

**Trust flow:**

```
founder Ed25519 key
        |
        | signs
        v
deploy/config/providers.toml (adds provider_id + provider_pubkey_ed25519)
        |
        | verifies
        v
provider Ed25519 key
        |
        | signs
        v
per-node attestation envelope
        |
        | verifies
        v
node Ed25519 + Dilithium3 keys sign HELLO
        |
        | verifies against attestation
        v
node admitted (reader_only role)
```

This gives founders coarse-grained control (which providers can sponsor at all) and providers fine-grained control (which nodes they attest to), without founders needing to sign every node individually.

### 3.4 Dynamic nginx upstream loader (Phase C - operational)

A small daemon (`rope-fleet-upstream-writer`) runs on rope-vps. Every 60 s:

1. Fetch the on-chain node registry via `rope_listStrings(kind=node)`.
2. Filter to nodes with `role_granted` in `{attester, sealer_candidate}` and no active `NodeRevoked` knot.
3. Emit a signed JSON file at `/opt/datachain-rope/fleet/upstreams.json`:
   ```json
   {
     "generated_at": 1786928234,
     "generation": 42,
     "readers": [
       { "host": "92.243.25.119", "port": 8545, "node_id": "..." },
       { "host": "157.230.18.45", "port": 8545, "node_id": "..." },
       ...
     ],
     "websocket": [ ... ],
     "primary_writer": { "host": "host.docker.internal", "port": 8545, "node_id": "..." },
     "signature": "<hex ed25519 sig from foundation operator key>"
   }
   ```
4. Render the nginx upstream blocks from this JSON via a Jinja-style template.
5. Verify with `docker exec rope-nginx nginx -t`.
6. If test passes AND generation is monotonically increasing, `nginx -s reload`.
7. If test fails, alert (do not reload, do not roll back - the old upstreams keep serving).

UFW rules are handled by the same daemon: it emits `/etc/ufw/rope-fleet.rules` and re-runs `ufw reload` when membership changes.

Guardrails:

- Reloads are rate-limited to 1 per 5 min.
- A generation must be signed by the foundation operator key AND at least one founder key (2-of-2).
- If the on-chain registry is unreachable for > 15 min, the daemon holds the previous upstreams and pages.
- A hand-drafted `upstreams.override.json` in the same directory (mode 600) takes precedence and disables auto-reload (operator escape hatch).

### 3.5 Revocation flow (Phase B / C)

Revocation is a Quipu Canon v1.1 tombstone (see `quipu-primitive-canon-v1.1.mdc`):

1. Founder signs a `NodeRevoked` message with `reason` + timestamp + node pubkey.
2. Any founder or guardian can submit `rope_appendToLedger(node_string_id, NodeRevoked)` (destructive-RPC-gated via the Phase-2 mechanism).
3. Node string status flips to `revoked`.
4. Upstream writer picks it up within 60 s and reloads nginx without the revoked node.
5. UFW loses the allow rule within the same window.

A revoked node's private key is *not* automatically destroyed - it just stops being accepted by the fleet. If the operator wants to permanently retire the key, they can do so out-of-band (shred + notify).

---

## 4. What we get in return

| Capability | Today | Phase A | Phase B | Phase C |
|---|---|---|---|---|
| Add a new attester without SSH-editing 4 VPSes | No | No | Requires HELLO + founder promote knot | Automatic once HELLO + promote lands |
| Accredited-cloud sponsorship | No | No | Yes (provider attestation) | Yes + auto-upstream |
| On-chain audit trail of every node event | No | Yes (registry visible) | Yes (HELLO/promote/revoke knots) | Same |
| Automatic revocation < 60 s | No (hand-edit) | No | Yes (from RPC) | Yes + nginx auto-drop |
| Cryptographic identity binding | No (IP+UFW) | Yes (registry check) | Yes (HELLO signature) | Same |
| Post-quantum ready | No | Yes (Dilithium3 in HybridPublicKey) | Yes | Yes |
| Operator overhead per new node | 1-2 hours (edit + restart) | 1-2 hours (same, plus registry knot) | 15 min (submit HELLO + promote knot) | 5 min (submit HELLO; nginx auto-picks-up) |

---

## 5. What we deliberately do NOT do

- **We do not implement a P2P gossip protocol for node membership.** The on-chain registry is the source of truth; gossip is a future v2 optimization if fleet size exceeds 100.
- **We do not automate sealer promotion.** `NodePromoted` from `attester` to `sealer_candidate` requires a founder-signed knot AND an operator opt-in step. See `WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` for why sealer transitions stay manual.
- **We do not replace UFW with app-level auth.** UFW stays; the upstream writer regenerates its rules. If a rogue IP bypasses UFW, the HELLO signature check + Phase-2 RPC gate still block state-changing operations.
- **We do not federate providers across chains.** A provider registered on Datachain Rope has no automatic standing on Ethereum, XDC, etc. Each chain has its own founders + provider registry if needed.

---

## 6. Effort estimate

| Phase | Scope | Effort | Depends on |
|---|---|---|---|
| A | On-chain `StringKind::Node` + `/api/v1/nodes` + fleet-status field | 2 engineer-weeks | Nothing (additive) |
| B | HELLO handshake + provider attestation + `providers.toml` + RPC + CLI + tests | 4 engineer-weeks | Phase A |
| C | Dynamic nginx upstream loader + UFW generator + rollback guards | 2 engineer-weeks | Phase B |

Total: **~8 engineer-weeks** for a fully-signed, self-service, PQ-ready node onboarding flow.

Comparison to today's cost: adding one new attester + wiring it into failover currently takes ~1 full day of coordinated ops work per node. At Phase C, the same operation is a self-service HELLO from the new node + a founder-signed `NodePromoted` knot; ~5 minutes of operator time.

---

## 7. Decision gates

Before scheduling Phase A:

- [ ] Confirm the operator wants an on-chain node registry (vs. staying on `master-nodes.toml` indefinitely - which is defensible if fleet stays at 4 nodes).
- [ ] Confirm the founder key roster is stable + rotation plan is documented (Phase-2 RPC pattern already supports founder key rotation; this design leans on it).
- [ ] Confirm ecosystem partners are not blocked on this (Datawallet+, Tanastok, DCSwap consume `master-nodes.toml` transitively via founder-signature verification; the on-chain registry does not break that).

Before scheduling Phase B:

- [ ] Confirm at least one accredited provider has agreed in principle to publish a `.well-known/dcrope-provider.json`.
- [ ] Confirm the Foundation is prepared to gate `providers.toml` (i.e., decide who is accredited).
- [ ] Confirm Phase A has been in production for at least 30 days with zero registry-consistency incidents.

Before scheduling Phase C:

- [ ] Confirm the dynamic-upstream daemon has been soak-tested against a staging nginx for at least 7 days.
- [ ] Confirm the escape-hatch (`upstreams.override.json`) works end-to-end.
- [ ] Confirm the 2-of-2 signature requirement (foundation operator + founder) is enforceable via existing key material.

---

## 8. Cross-references

- `crates/rope-consensus/src/validator_registry.rs` - existing identity primitives
- `crates/rope-crypto/src/hybrid.rs` - HybridPublicKey (Ed25519 + Dilithium3)
- `crates/rope-node/src/rpc_signature.rs` - domain-tag + replay-store construction (HELLO reuses this pattern)
- `crates/rope-node/src/governance.rs` - founder key loading + attestation verification
- `.cursor/rules/handover-security-audit-2026-06-11.mdc` - V11 destructive-RPC gate (HELLO promotion goes through this)
- `.cursor/rules/quipu-canon-v1.2-string-registry.mdc` - 5 canonical kinds (this design adds a 6th)
- `WRITER_PROMOTE_RUNBOOK_AND_TIER_D_ROADMAP_2026-08-23.md` - companion doc; sealer promotion is intentionally NOT automated by this design

---

## 9. Open questions for the operator

1. **Founder key rotation cadence.** Currently rotate-on-suspicion. Do we want a scheduled cadence (e.g. every 12 months) once the registry is on-chain?
2. **Provider slate.** Which accredited providers should we approach first? Exoscale (mentioned by operator), Databox (mentioned by operator), OVHcloud, Scaleway? A short-list of 3-5 is enough for Phase B.
3. **Sealer-candidate quorum.** How many attesters must a node run as before it is eligible for sealer promotion? The v2 architecture spec implies 21 validators; this design defaults to that but does not enforce.
4. **Legacy fleet migration.** The current 4 nodes (BLUE, GREEN, DO-1, DO-2) must be back-registered under Phase A. This is a one-time knot-append per node; low risk but should be scheduled.
5. **`master-nodes.toml` deprecation.** Once Phase C is live for 90 days, do we deprecate `master-nodes.toml` entirely, or keep it as a break-glass local file that always overrides the on-chain registry?

---

*This document is the source of truth for cryptographic node onboarding. It does not change any running code today. Phase A ships only after the operator answers §9.*
