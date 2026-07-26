# Quipu Primitive Canon: Knot Hash Construction (§6.1.1 alignment)

**Status.** Specification implemented in code as the new module
`crates/rope-core/src/knot_hash.rs`. Production `RopeString::compute_id`
retains the v1 construction. Migration to the v2 (§6.1.1) construction is
scheduled for the next Quipu Primitive Canon revision (provisionally v1.3).

**Date.** 2026-05-07
**Spec source.** §6.1.1 of `papers/Datachain_Rope_Quipu_Proto_Computer_Anthropological_Paper.md`

> **Companion document (2026-05-09).** This note is the **fork-required**
> migration plan (Phases 1, 2, and 3 are a soft fork). The
> **non-forking** alternative is documented in
> `KNOT_HASH_V2_WITNESS_SHADOW_DESIGN.md`, which delivers §6.1.1
> properties via a witness-local shadow chain with IPFS bulletin and
> optional v1-chain anchor knots, requiring zero protocol-level
> change. Read both documents and choose the path that fits the
> Foundation's appetite at the time of decision.

---

## 1. Why this note exists

The §6.1.1 specification (paper) introduces the formal cryptographic
construction by which `rope_untieKnot` preserves chain continuity under
granular erasure: a per-knot hash chain over erasure-survivable fields, with
the encrypted payload committed separately under OES.

The current `RopeString::compute_id` in
`crates/rope-core/src/string.rs` (lines 241 to 266) implements the v1.0/v1.1
construction:

```text
StringId = BLAKE3(σ || τ || π || ρ || μ)
```

The sequence σ (the content / payload bytes of the knot) is in the identity
pre-image. Under destruction of the OES key shreds, the ciphertext bytes of σ
remain on disk, but the construction does not formally separate the
*durability commitment* (chain continuity) from the *confidentiality
commitment* (payload recoverability) the way §6.1.1 requires.

A peer reviewer of the paper checking the codebase against §6.1.1 will
observe this gap. This note documents the gap explicitly, the migration
plan, and the public commitment that ships alongside the academic paper.

## 2. The two constructions

### 2.1 v1 (current production)

```text
StringId = BLAKE3(σ || τ || π || ρ || μ)
```

- Lives in `RopeString::compute_id` and `StringId::from_content`.
- σ = `NucleotideSequence` (the content bytes of the knot).
- τ = `LamportClock` (the temporal marker).
- π = parentage (the predecessor `StringId`s in the DAG).
- ρ = replication factor.
- μ = mutability class discriminant.
- Used by every existing string on the production network.

### 2.2 v2 (§6.1.1 spec, implemented in code today)

```text
event_metadata_hash = BLAKE3(
    "DCROPE/quipu-canon/event-metadata-hash/v1" ||
    timestamp_bytes ||
    witness_ids || testimony_quorum ||
    oes_key_shred_destinations
)

h_i = BLAKE3(
    "DCROPE/quipu-canon/knot-hash-chain/v1" ||
    event_id || event_type ||
    event_metadata_hash || authorisation_proof ||
    h_{i-1}
)
```

- Lives in `crates/rope-core/src/knot_hash.rs`.
- The encrypted `event_payload` is **not** in the pre-image. It is committed
  separately under the OES per-knot ephemeral key.
- The `event_metadata_hash` commits to the OES key-shred destination set,
  so the audit trail can verify post hoc which witnesses held shreds for an
  erased knot and whether their destruction obligations were honoured.
- The construction supports the tombstone property of §6.1.1 directly:
  the fields entering `h_i` survive the destruction of the OES key shreds
  for k_i, so successor knots `k_{i+1}, k_{i+2}, ...` continue to verify
  without any re-hashing of the tail of the string.

## 3. Migration plan

The plan preserves backward compatibility throughout. v1 strings keep their
v1 identities forever; v2 is opt-in for new knots and becomes the default
for new knots after the network soft fork.

### Phase 0: Spec available in code (now, May 2026)

- §6.1.1 spec exists as `crates/rope-core/src/knot_hash.rs`.
- Tests cover: hash determinism, sensitivity to each pre-image field,
  tombstone preservation of chain continuity, payload independence,
  metadata-hash sensitivity to OES key-shred destinations, domain
  separation between metadata-hash and chain-hash callers.
- No call site is migrated yet. The public API is available for new code
  paths that need the §6.1.1 properties.

### Phase 1: Canon vNext spec freeze (target Q3 2026)

- Quipu Primitive Canon vNext (provisionally v1.3) freezes the §6.1.1
  construction as the normative knot-hash construction.
- The Canon source in `.cursor/rules/quipu-primitive-canon-v1.1.mdc` is
  superseded by `quipu-primitive-canon-v1.3.mdc`, which embeds §6.1.1 in
  §4 and §5 of the Canon proper.

### Phase 2: Implementation alignment in node code (target Q4 2026)

- `RopeString` gains a `knot_hash_version: u8` field (default `1` for
  backward compat).
- A new builder path, `RopeStringBuilder::with_canon_v2_hash(...)`, opts
  into the §6.1.1 construction. Under v2, the StringId for a new knot is
  the `KnotHash` of the §6.1.1 chain, σ is no longer in the identity
  pre-image, and the encrypted payload is bound to the knot via the
  OES wrapping (which is independently verifiable but is not on the
  integrity-critical hash path).
- All call sites that create new strings under the v2 Canon are routed
  through the v2 path.
- Existing v1 strings retain their v1 StringIds. The network supports
  both, with the version byte distinguishing them.

### Phase 3: Network migration (target 2027)

- A network-wide soft fork (negotiated between knot witnesses) advances
  the default new-knot version to 2.
- v1 knots remain valid forever for chain continuity. The v1 verification
  algorithm continues to work; only new knots use v2.
- Documentation across DCScan, Datawallet+, DCSwap, Tanastok references
  the v2 construction.

## 4. Backward compatibility

The plan is non-breaking at every step:

- v1 knots remain verifiable with the v1 algorithm.
- v2 knots use the §6.1.1 construction.
- A version byte in serialised knots distinguishes the two constructions.
- Existing string identifiers do not change.
- EVM compatibility is unaffected (the EVM execution path uses the Reth
  backend, not the cord-native path; Reth's block-hash construction is
  a separate concern).

## 5. Risk register

| Risk | Mitigation |
|---|---|
| v2 path has a bug v1 does not | v2 is gated behind explicit opt-in until the network soft fork. The testing window covers at least one full Canon revision cycle. |
| Confusion between v1 and v2 hashes in audit tooling | Domain-separation tags (`DCROPE/quipu-canon/knot-hash-chain/v1` and `DCROPE/quipu-canon/event-metadata-hash/v1`) are part of the §6.1.1 construction and make the result self-identifying. |
| Ecosystem agents (DCSwap, Tanastok, Datawallet+) assume immutability of the v1 identifier construction | Handover rule documents the v2 migration; agents are notified ahead of any default change via the standard ecosystem-string-emission handover mechanism. |
| Performance regression from v2 path | v2 is structurally similar to v1 (one BLAKE3 hash per knot, plus one BLAKE3 hash per metadata commit). Expected per-knot overhead is below 200 nanoseconds on contemporary x86-64 CPUs. Benchmarked under the Quipu Canon v2.0 Phase 1 harness before default cutover. |
| Protocol-level confusion: which construction is in force at chain level? | The on-chain knot record carries an explicit `knot_hash_version` byte; verifiers consult this byte before computing the verification hash. Mixed-version verification is supported by construction. |

## 6. References

- `papers/Datachain_Rope_Quipu_Proto_Computer_Anthropological_Paper.md` §6.1.1
  (the formal specification of the construction)
- `.cursor/rules/quipu-primitive-canon-v1.1.mdc` (the Canon document this
  note targets for revision)
- `.cursor/rules/quipu-canon-knot-hash-construction.mdc` (the rule pinning
  the §6.1.1 spec as the v2 target and prescribing agent behaviour during
  the migration window)
- `crates/rope-core/src/knot_hash.rs` (the v2 implementation)
- `crates/rope-core/src/string.rs` lines 241 to 266 (the v1 `compute_id`)
- `crates/rope-core/src/types.rs` line 12 (the v1 `StringId` formula)

## 7. Tracking

This note is the canonical migration record. It is updated when each
phase completes. Sub-tasks for Phase 1 and Phase 2 are tracked in the
relevant Quipu Canon revision branch on the rope-node repository under
the label `canon/knot-hash-v2`.
