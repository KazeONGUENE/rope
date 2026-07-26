# Knot Hash v2 (§6.1.1): The Witness-Shadow Non-Forking Path

**Author.** Datachain Foundation engineering, after the in-depth analysis on 2026-05-09
**Status.** Architectural memo. Not yet authorised for implementation.
**Companion to.** `QUIPU_CANON_KNOT_HASH_CONSTRUCTION.md` (the fork-required migration plan)
**Spec source.** §6.1.1 of `papers/Datachain_Rope_Quipu_Proto_Computer_Anthropological_Paper.md`

---

## 0. Why this memo exists

The companion document `QUIPU_CANON_KNOT_HASH_CONSTRUCTION.md` records the
fork-required migration plan for aligning the implementation with the
§6.1.1 specification. Its Phase 2 changes the `RopeString` field layout
and Phase 3 is a network soft fork. Both are real fork events with the
operational and ecosystem-coordination cost they imply. The user's standing
instruction is **evolution not revolution**: deliver §6.1.1 properties to
the network without forking the chain.

This memo finds the path. It defines what counts as a fork, surveys the
non-fork design space, evaluates four candidates, and recommends a single
hybrid architecture (the *Witness-Shadow Chain*) that delivers the §6.1.1
properties with zero protocol-level change. It also names, honestly, what
this approach can and cannot do, so that the recommendation is choosable
rather than oversold.

---

## 1. The constraint: what is a fork on Datachain Rope

The Datachain Rope mainnet (chain ID 271828) consists of EVM execution
under Reth, a `rope-node` consensus layer with Testimony witnesses, and
the cord-anchored knot-on-string structure documented in the Quipu Canon.
Any change to this system is one of three categories.

| Category | What it is | Example | Acceptable here? |
|---|---|---|---|
| **Hard fork** | A consensus rule change that splits the network into pre-fork and post-fork chains; pre-fork nodes reject post-fork blocks and vice versa | Increase block gas limit; change the canonical signature scheme | **No** |
| **Soft fork** | A consensus rule tightening; pre-fork nodes accept post-fork blocks but post-fork nodes reject some pre-fork blocks | Change the default `knot_hash_version` from 1 to 2 | **No** |
| **Additive evolution** | New optional capabilities that any subset of nodes can opt into; no consensus rule change; no canonical-chain rewrite | New advisory RPC method; new sidecar service; new derived index | **Yes** |

The Phase 2 work in the companion document is on the soft-fork boundary
(adding a version byte to a consensus-validated struct). Phase 3 is an
explicit soft fork. Both are out of scope under the **evolution** constraint.

This memo restricts itself to the third category: any architecture in
which a node that does not opt in remains a fully valid participant in
the canonical chain, and any architecture in which the canonical chain
hash, the canonical state root, the canonical EVM behaviour, and the
canonical RPC surface are unchanged.

---

## 2. The design space: four candidate architectures

I examined four architectures that could deliver §6.1.1 properties without
a fork. They are not mutually exclusive and the recommended path combines
elements from three of them.

### 2.1 Architecture A: Off-chain attestation service

A separate process subscribes to the canonical chain over the public RPC,
recomputes the §6.1.1 hash for each new knot from the publicly visible
fields (`event_id`, `event_type`, witness signatures, OES key-shred
metadata where exposed), and stores the resulting v2 chain in its own
database. It exposes a parallel RPC (`v2.knothash.rdp.tools/...` or
similar) that returns the v2 chain commitment on request.

| Property | Verdict |
|---|---|
| Fork? | None. The canonical chain is not modified. |
| §6.1.1 chain-continuity-under-erasure delivered? | Partially. The v2 chain is verifiable but only the operator can attest to its history; another party running the same service computes the same v2 chain (the construction is deterministic from public inputs) but they have no way to know the operator's history was not selectively rewritten. |
| Cost | Low. One Rust binary, ~2,000 lines, one database, one new public endpoint. |
| Risk to canonical chain | Zero. |
| Trust model | Centralised on the operator unless the v2 chain is itself anchored. |

### 2.2 Architecture B: Per-knot dual-commit (in `RopeString`)

Each new knot, when written by the proposer, computes both the v1
`StringId = BLAKE3(σ || τ || π || ρ || μ)` and a v2
`KnotHash = BLAKE3(event_id || event_type || event_metadata_hash ||
authorisation_proof || h_{i-1})`. Both are stored on `RopeString`.
Witnesses that opt in additionally validate the v2 hash. Witnesses that
do not opt in validate only the v1 hash.

| Property | Verdict |
|---|---|
| Fork? | Adds a field to `RopeString`. Bincode (which is what the codebase serialises with) is positional, not self-describing; a field addition breaks deserialisation of any pre-existing serialised `RopeString`. This is, in practice, a hard-fork of the on-disk format even if the consensus rules are unchanged. |
| §6.1.1 delivered? | Strongly: the v2 hash is on every new knot, in the canonical position. |
| Cost | Medium. Field addition + storage migration + consumer updates. |
| Risk | High: the on-disk format break propagates to every component that reads serialised `RopeString` (the lattice store, the RDP piece map, the IPFS replication path, the explorer indexer). |
| Trust model | Strong: v2 hash is co-canonical with v1. |

This architecture is what the companion document's Phase 2 describes. It
is excluded from the present memo on the **evolution** test.

### 2.3 Architecture C: Witness-Shadow Chain (in-node, advisory)

Every `rope-node` validator that opts in maintains a local v2 shadow
chain in its own storage, computed in parallel with the canonical v1
chain from the same inputs the v1 chain consumes. The shadow chain is
**not** part of the canonical state. It is a derived, deterministic,
locally-cached view that any opted-in witness can compute independently
from the same canonical inputs. The shadow chain is exposed via new
advisory RPC methods (`rope_v2KnotHash`, `rope_v2WalkChain`) and is not
required for consensus participation.

| Property | Verdict |
|---|---|
| Fork? | None. Witnesses that do not enable the shadow do not need any code change at all (the feature is gated). |
| §6.1.1 delivered? | The v2 hash is computed for every new knot by every opted-in witness. Multiple independent witnesses computing the same v2 chain produce identical results (the construction is deterministic), so cross-witness cross-checking is automatic. Verifiability post-erasure is preserved by construction. |
| Cost | Medium. New crate or module in `rope-node`; subscribes to existing `LedgerLifecycleEvent::EntryAppended` events; adds two RPC methods; one new RocksDB column family. |
| Risk to canonical chain | Zero. The shadow chain is read-only from the canonical chain's perspective. |
| Trust model | Decentralised by construction. Anyone can run a shadow witness. |

### 2.4 Architecture D: Application-layer adoption

Ecosystem agents that benefit from §6.1.1 properties (Datawallet+ for
inheritance, Tanastok for ERC-3643 audit, NaturaProof for biodiversity
verification chains) import the `knot_hash` module from `rope-core` and
use the v2 construction at their own application layer. They build
their own per-application v2 chains over their own knots, anchored
periodically into the canonical chain via regular attestation knots.

| Property | Verdict |
|---|---|
| Fork? | None. Application layer only. |
| §6.1.1 delivered? | Only for application-domain knots, not for arbitrary v1 knots. |
| Cost | Low per application; high in aggregate if every application reinvents the wheel. |
| Risk | Application-specific bugs do not affect canonical chain. |
| Trust model | Application-specific. |

---

## 3. Recommendation: the hybrid Witness-Shadow architecture

The cleanest non-forking path combines C, A, and D in a single coherent
architecture I will call the **Witness-Shadow Chain**.

### 3.1 Components

1. **In-node shadow chain (Architecture C, the core)**: every `rope-node`
   that opts in maintains a v2 shadow chain in a separate RocksDB column
   family, computed from `LedgerLifecycleEvent::EntryAppended` and from
   `rope_untieKnot`-emitted tombstone events. The shadow chain consumes
   the same inputs the canonical chain consumes; the canonical chain is
   not modified.

2. **Public IPFS bulletin (a hardening of Architecture A)**: every
   anchor-interval (~3 seconds), the in-node shadow chain publishes a
   Merkle root of its recent v2 commitments to the existing IPFS
   three-node mesh (per `reth-blue-green-ipfs-architecture.mdc`). The
   IPFS CID becomes a public, content-addressed reference for the v2
   chain at that anchor interval. Any party can pin and verify it.

3. **Optional v1-chain anchoring (additive, no fork)**: the shadow
   witness optionally writes a regular knot to its own string of
   `event_type = "v2_anchor"` containing the IPFS CID of the latest
   shadow-chain Merkle root. This anchors the v2 chain into the
   canonical chain without any consensus-rule change: the anchor knot
   is just a normal knot from the protocol's perspective.

4. **Application-layer consumers (Architecture D)**: Datawallet+,
   Tanastok, NaturaProof, and DCSwap query the new advisory RPC
   methods to get the v2 chain commitments for the strings they care
   about. DCScan optionally displays the v2 chain alongside the v1
   chain in its address page for educational and audit purposes.

### 3.2 Why this delivers §6.1.1

Each of the §6.1.1 invariants is delivered by the architecture:

- **The v2 hash is computed over erasure-survivable fields only**: the
  shadow chain uses the `compute_knot_hash` function from
  `crates/rope-core/src/knot_hash.rs`, which by construction excludes
  the encrypted payload from the pre-image.
- **Chain continuity under granular erasure**: the shadow chain reads
  the v1 erasure events from the lifecycle stream
  (`LedgerLifecycleEvent::LedgerDeleted` and the per-knot tombstone
  events). When a knot is untied, the shadow chain records the
  tombstone using `tombstone_preimage` and continues to chain forward
  without re-hashing the tail.
- **Separation of durability and confidentiality commitments**: by
  construction, the shadow chain never stores or hashes the encrypted
  payload. It commits only to the metadata.
- **Audit trail of OES key-shred destruction**: the shadow chain's
  `event_metadata_hash` includes the OES key-shred destination set,
  which it learns from the lifecycle stream's
  `record_append(... oes_generation ...)` event and from a new
  per-knot field that the shadow witness adds to its local model
  (the canonical knot does not need to carry this field; the shadow
  derives it from observable OES generation state).

### 3.3 Why this is not a fork

| Test | Result |
|---|---|
| Does a witness without the shadow code remain fully valid in consensus? | Yes. The shadow is a local-only computation. |
| Does the canonical chain hash change for any knot? | No. The v1 `StringId = BLAKE3(σ || τ || π || ρ || μ)` is unchanged. |
| Does the canonical state root change? | No. |
| Does any consumer break? | No. New methods are additive. |
| Does every existing on-chain string keep its current identifier? | Yes, forever. |
| Does the EVM execution path change? | No. The Reth backend is untouched. |
| Does the cord-anchor commitment scheme change? | No. The optional `v2_anchor` knots are normal knots from the protocol's perspective. |

The Witness-Shadow Chain passes all seven non-fork tests.

---

## 4. Implementation sketch

Implementation is grounded in components that already exist in the
codebase. Nothing in this sketch is hypothetical infrastructure; every
hook is a real one I have located in the source tree.

### 4.1 New crate or module

The shadow chain lives in a new module `crates/rope-node/src/shadow_chain.rs`
(or a sibling crate `crates/rope-shadow-chain/` if the user prefers
separation of concerns). It re-uses the existing
`crates/rope-core/src/knot_hash.rs` types and functions:

- `KnotHash`, `KnotHashPreImage`, `EventMetadataHash`, `EventMetadata`
  (already implemented and tested with 20 unit tests).
- `compute_knot_hash`, `compute_event_metadata_hash`, `tombstone_preimage`
  (already implemented).

The shadow chain adds:

- `ShadowChain` struct: holds per-string head `KnotHash` and a chain log.
- `ShadowChainStore`: a RocksDB column family adapter (uses the same
  RocksDB instance the rest of `rope-node` uses; new column family).
- `ShadowChainObserver`: subscribes to `LedgerLifecycleEvent` (the enum
  in `crates/rope-protocols/src/ledger_lifecycle.rs`) and updates the
  shadow on `EntryAppended` and on tombstone events.
- `ShadowChainAnchor`: optional component that publishes Merkle roots
  to IPFS and writes `v2_anchor` knots to the canonical chain.

### 4.2 Subscription to the canonical chain

The hook is already there. In `crates/rope-node/src/ledger_manager.rs`,
the `append_to_ledger` method calls `self.lifecycle.record_append(...)`
(lines 385 to 391 of that file). The `LedgerLifecycleManager` already
maintains an event log. The shadow chain observer subscribes to that
event log (or to a broadcast channel fed by it; if the existing log is
poll-only, a tokio broadcast channel is added in front of it as a
small additive change to `LedgerLifecycleManager`).

`rope_untieKnot` similarly emits tombstone events through the same
lifecycle channel.

### 4.3 New advisory RPC methods

Two methods, both prefixed `rope_v2_` to make their advisory status
explicit and to leave the canonical `rope_*` namespace untouched:

```text
rope_v2_knotHash(string_id, event_id) -> { knot_hash, event_metadata_hash }
rope_v2_walkChain(string_id, offset, limit) -> [ {event_id, knot_hash, event_metadata_hash, prev_hash} ]
```

The methods are added to `crates/rope-node/src/rpc_server.rs` next to
the existing `rope_appendToLedger`, `rope_untieKnot`, and
`rope_walkString` handlers. They are guarded by a config flag
(`shadow_chain.enabled = true` in `deploy/config/rope-witness.toml`)
so that non-shadow witnesses do not pay the cost.

### 4.4 IPFS publication

The IPFS three-node mesh exists in production per
`reth-blue-green-ipfs-architecture.mdc`. The shadow chain anchor
publishes a `ShadowChainSnapshot` to IPFS every anchor interval,
containing:

- The Merkle root of all v2 chain heads at the snapshot time
- The set of strings touched in this interval
- The witness identifier and signature
- The previous snapshot's CID

The anchor publishes the CID to the IPNS path
`/ipns/<witness>/v2-shadow/latest`. Independent verifiers pin the IPNS
record and walk the snapshot graph backward.

### 4.5 Optional v1-chain anchoring

The shadow witness writes a regular knot to its own string with:

- `event_type = "v2_anchor"`
- payload (encrypted under the shadow witness's own OES key) containing
  the IPFS CID of the latest snapshot

This is just a normal knot from the canonical chain's perspective. No
protocol code path special-cases it. Verifiers that care can walk the
shadow witness's string and find the anchor knots.

### 4.6 Storage cost

Per knot, the shadow chain stores: 32 bytes `KnotHash`, 32 bytes
`EventMetadataHash`, 8 bytes `event_id`, ~16 bytes string id reference,
plus the (variable) authorisation_proof (currently empty since
`verify_signatures: false`; see §5.1 below). Approximate per-knot
cost: 100 bytes. At the current production rate of ~1,500 knots per
second sustained, this is ~150 KB per second, ~13 GB per day per
shadow witness. With RocksDB compaction, sustained cost is well
within the existing 200 GB SSD floor for a knot witness (per §10.4
of the paper).

### 4.7 Estimated effort

| Phase | Effort | Risk |
|---|---|---|
| Design review and config schema | 1 day | None |
| `ShadowChain` and `ShadowChainStore` implementation | 3 days | Low |
| `ShadowChainObserver` wiring into `LedgerLifecycleManager` | 2 days | Low |
| RPC methods and tests | 2 days | Low |
| IPFS publication and IPNS path | 2 days | Medium (operational) |
| Optional `v2_anchor` knot writer | 1 day | Low |
| End-to-end integration test (single VPS) | 2 days | Low |
| Canary deployment to `datachain-rpc-1` (DigitalOcean) | 1 day | Low (it is the tertiary follower) |
| Soak test, observe for 7 days | 7 days wall clock, ~0 active hours | Low |
| Promotion to GREEN, then BLUE, with rollback gate at each stage | 2 days | Medium |

Total active engineering: ~16 days. Total wall-clock to GREEN canary
running cleanly: ~3 weeks. Total wall-clock to all three VPS running
shadow chain: ~5 weeks.

---

## 5. Honest limitations

Every architecture has a price; this memo names the price up front.

### 5.1 The v2 chain is only as strong as the v1 inputs it observes

`authorisation_proof` is, in the §6.1.1 spec, the post-quantum
signature of the authorising party. In production today,
`verify_signatures: false` is the consensus configuration (see line 119
of `crates/rope-node/src/consensus_orchestrator.rs`), so signatures
on knots are present but not currently verified. The shadow chain
therefore has two options:

1. **Use the existing signature anyway.** The shadow chain hashes the
   signature bytes that ride on the canonical knot. If the signature
   is empty (placeholder), the hash is over an empty pre-image element,
   which is weaker than §6.1.1 envisions. **This is the same trust level
   as v1**: the shadow chain is no weaker than the canonical chain it
   observes.
2. **Wait for `verify_signatures: true`.** The Quipu Canon v2.0
   throughput-scaling roadmap (Phase 2 there, target Q4 2026) turns on
   signature verification. Until then, the shadow chain is honest about
   inheriting the v1 trust level.

Either choice is defensible. Option 1 ships sooner; option 2 ships at
full §6.1.1 strength. The recommendation is option 1, with documentation
that the shadow chain inherits v1 trust until the Phase 2 of the
throughput roadmap lands.

### 5.2 The OES key-shred destination set is an architectural target, not a current materialisation

§6.1.1's `event_metadata_hash` includes the OES key-shred destination
set. In production today, the OES key derivation is per-`(wallet, generation)`
(see `crates/rope-node/src/ledger_manager.rs` line 343 to 345), and the
destination set for the shred-distribution phase exists in OES code but
is not currently exposed as a per-knot field on the canonical knot.
The shadow chain has three options:

1. **Approximate from witness signatures**: assume the destination
   set is the witness set that signed the cord-anchor for the
   knot's interval. This is a defensible approximation and is
   verifiable from public chain state.
2. **Add an additive metadata field**: extend the canonical knot's
   metadata with the OES destination set as a new optional field.
   Bincode-serialisation-incompatible per §2.2 above. Excluded.
3. **Compute from OES generation state**: the OES generation contains
   enough state to recover the destination set deterministically; the
   shadow chain computes it locally from the OES generation and the
   wallet identifier.

Option 3 is the cleanest and requires no canonical-knot field change.
Option 1 is the fallback if option 3 turns out to require OES code
exposure that the OES module does not currently provide.

### 5.3 The shadow chain is advisory, not consensus-enforced

A shadow witness that lies about a v2 hash is detectable but not
slashed. Detection works because every other shadow witness, computing
deterministically from the same canonical inputs, produces the same
v2 chain. A regulator who sees disagreement between two shadow
witnesses can flag either as faulty. But there is no on-chain economic
penalty.

This is a feature, not a bug, of the non-forking constraint: enforcing
a v2 hash in consensus would require Phase 3 of the companion document,
which is the network soft fork the user explicitly rules out.

### 5.4 Retroactive coverage of historical v1 strings is bounded

A new shadow witness coming online observes only events from its
subscription start. Computing the v2 chain for historical v1 knots
requires replay from the canonical chain. This is operationally
feasible (the replay is BLAKE3 plus some metadata reconstruction;
estimated 30 minutes for the entire 1.1M-anchor history at the time of
writing) but it is an operational step, not a free property.

### 5.5 IPFS publication is an additional dependency

The shadow chain's IPFS bulletin depends on the existing IPFS three-node
mesh remaining online. If the mesh is partitioned, the shadow chain
continues to compute v2 commitments locally (no liveness impact on the
canonical chain) but the public bulletin lags. This is consistent with
the existing operational model of the IPFS mesh per
`reth-blue-green-ipfs-architecture.mdc`.

---

## 6. What this memo does not promise

To be explicit about scope:

- **It does not promise that the canonical chain will adopt the v2
  construction.** That is Phase 2 + Phase 3 of the companion document
  and remains scheduled for Q4 2026 to 2027 if the Foundation chooses
  to proceed.
- **It does not promise a consensus-enforced v2 chain.** The shadow
  chain is advisory by design; making it consensus-enforced would be a
  fork.
- **It does not promise zero operational cost.** Three weeks of
  wall-clock effort for canary deployment to a single VPS, with five
  weeks to all three VPS, is the realistic estimate.
- **It does not promise §6.1.1 properties stronger than v1 inherits.**
  Today the v1 chain inherits the trust level of `verify_signatures:
  false`; the shadow chain inherits the same level until the throughput
  roadmap's Phase 2 lands.

What it does promise: a deployable, non-forking, decentralised,
deterministically-verifiable, regulator-checkable, IPFS-anchored,
optionally chain-anchored realisation of the §6.1.1 construction over
the canonical Datachain Rope mainnet, available to any consumer that
wants it, requiring no consumer migration, no validator coordination,
and no chain rewrite.

---

## 7. Decision points for the Foundation

Three explicit decisions are required before implementation begins.

### 7.1 Crate location

Should the shadow chain be a module in `rope-node` (`shadow_chain.rs`)
or a separate sibling crate (`rope-shadow-chain`)? The module form
ships faster; the crate form is cleaner and easier to operate as an
independent process on a non-witness machine (e.g. a regulator's own
infrastructure).

**Recommendation**: separate crate. The capability is properly an
independent piece of infrastructure (it can be operated by parties
that do not run a knot witness, such as regulators or auditors), and
the separation makes that operational independence clear.

### 7.2 OES key-shred destination materialisation

Option 1 (approximate from witness signatures) or Option 3 (compute
from OES generation state)?

**Recommendation**: Option 3. It is closer to the §6.1.1 spec and
does not require renaming the meaning of any canonical chain object.

### 7.3 IPNS path namespace

The shadow chain's IPFS bulletin needs a stable namespace. Options:

1. `/ipns/<shadow_witness_pubkey>/v2-shadow/latest` (per-witness).
2. `/ipns/datachain-foundation/v2-shadow/latest` (Foundation-canonical
   single source).
3. Both: per-witness for verifiability, Foundation aggregation for
   convenience.

**Recommendation**: option 3.

---

## 8. Cross-references

- `papers/Datachain_Rope_Quipu_Proto_Computer_Anthropological_Paper.md` §6.1.1
  (the formal specification of the construction)
- `datachain-rope/docs/QUIPU_CANON_KNOT_HASH_CONSTRUCTION.md` (the
  fork-required migration plan; this memo is its non-forking sibling)
- `datachain-rope/crates/rope-core/src/knot_hash.rs` (the v2
  construction, already in code, with 20 passing unit tests; this memo
  promotes it from a callable library function to a deployable
  shadow-chain service)
- `datachain-rope/crates/rope-protocols/src/ledger_lifecycle.rs` (the
  `LedgerLifecycleEvent` enum the shadow chain subscribes to)
- `datachain-rope/crates/rope-node/src/ledger_manager.rs` (the
  `append_to_ledger` and `untie_knot` paths the shadow chain observes)
- `.cursor/rules/reth-blue-green-ipfs-architecture.mdc` (the IPFS mesh
  the shadow chain publishes to; if missing under that filename, the
  rule referenced from `reth-blue-green-ipfs-architecture` in
  handovers)
- `.cursor/rules/quipu-canon-knot-hash-construction.mdc` (the
  always-applied rule pinning the §6.1.1 spec; this memo is consistent
  with that rule and adds the non-fork deployment path)

---

## 9. Status of this memo

This memo is, at the time of writing, a recommendation pending
authorisation. It represents the in-depth analysis the Foundation
asked for on 2026-05-09 of how to deliver §6.1.1 properties without
forking the chain. The next step is a Foundation decision on the three
decision points in §7 and on whether to authorise the implementation
on the timeline of §4.7. Until that authorisation, no code change beyond
what shipped on 2026-05-07 (the `knot_hash` module and its tests) is
made to the codebase, no service is deployed, and no canonical chain
behaviour is altered.

---

*Prepared by Datachain Foundation engineering. This memo will be
updated to reflect the Foundation's decisions on §7 and the
authorisation status, and will be promoted from "architectural memo"
to "deployment plan" if authorised.*
