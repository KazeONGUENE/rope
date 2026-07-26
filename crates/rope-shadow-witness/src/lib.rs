//! # rope-shadow-witness
//!
//! Quipu Primitive Canon §6.1.1 shadow-chain witness.
//!
//! ## What this crate is
//!
//! `rope-shadow-witness` is an off-chain process that observes the
//! canonical Datachain Rope chain via the public `rope_*` JSON-RPC
//! interface, computes the §6.1.1 v2 knot-hash chain (the durability
//! commitment defined in `rope_core::knot_hash`) over each observed
//! knot, persists the resulting v2 chain in a local RocksDB store, and
//! exposes it on a separate JSON-RPC port via two advisory methods:
//!
//! - `rope_v2_knotHash(string_id, event_id)`
//! - `rope_v2_walkChain(string_id, offset, limit)`
//!
//! ## What this crate is NOT
//!
//! It is not a consensus participant. It does not modify the canonical
//! chain. It does not require any change to `rope-node`, the Quipu
//! Canon, or any other deployed binary. It is a *non-forking* path to
//! delivering §6.1.1 properties to the network, per
//! `docs/KNOT_HASH_V2_WITNESS_SHADOW_DESIGN.md`.
//!
//! ## v0.1 fidelity scope
//!
//! Because the shadow witness consumes only public RPC, the
//! `EventMetadata` it constructs for each knot uses observable proxies
//! for fields that the canonical RPC does not yet expose:
//!
//! | §6.1.1 field | v0.1 source | Future hardening |
//! |---|---|---|
//! | `event_id` | `knot_index` from `rope_getStringWithKnots` | Same |
//! | `event_type` | `"append"` (active) or `"erasure"` (tombstone) | Extended controlled vocabulary |
//! | `timestamp_bytes` | empty for active; tombstone `untied_at` big-endian for erasure | RPC extension exposing knot timestamp |
//! | `witness_ids` | empty | RPC extension exposing per-knot signature set |
//! | `testimony_quorum` | 0 | RPC extension exposing consensus rule |
//! | `oes_key_shred_destinations` | empty | OES module exposure |
//! | `authorisation_proof` | empty for active; tombstone `audit_hash` for erasure | RPC extension or in-process subscription |
//!
//! The §6.1.1 chain-continuity-under-erasure property is preserved at
//! v0.1: tombstones are applied via `rope_core::knot_hash::tombstone_preimage`,
//! and successor-knot continuity does not depend on the proxied fields.
//!
//! ## Layout
//!
//! - [`config`] — TOML configuration schema.
//! - [`error`] — error type for all surfaces.
//! - [`client`] — JSON-RPC client polling the canonical `rope-node`.
//! - [`store`] — RocksDB persistence of the v2 chain.
//! - [`chain`] — v2 chain core: applies observed knots and tombstones.
//! - [`observer`] — poll loop coordinating client, chain, and store.
//! - [`server`] — JSON-RPC server exposing `rope_v2_*` methods.

pub mod chain;
pub mod client;
pub mod config;
pub mod error;
pub mod observer;
pub mod server;
pub mod store;

pub use error::{ShadowWitnessError, ShadowWitnessResult};

use serde::{Deserialize, Serialize};

use rope_core::knot_hash::{EventMetadataHash, KnotHash};

/// One entry in the v2 shadow chain.
///
/// Stored under key `(string_id || event_id_be_bytes)` in the chain
/// column family of the shadow store.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowChainEntry {
    /// String identifier (hex-encoded 32 bytes) the entry belongs to.
    pub string_id: String,
    /// `event_id` per §6.1.1 (the position of the knot in its string).
    pub event_id: u64,
    /// `event_type`. `"append"` for an active knot, `"erasure"` for a
    /// tombstone, future-extensible to the full controlled vocabulary.
    pub event_type: String,
    /// `event_metadata_hash`. BLAKE3 commitment over the metadata.
    pub event_metadata_hash: EventMetadataHash,
    /// `h_i`. The §6.1.1 chain hash for this knot.
    pub knot_hash: KnotHash,
    /// `h_{i-1}`. The chain hash of the predecessor knot.
    pub previous_hash: KnotHash,
    /// Whether the underlying canonical knot is a tombstone at the
    /// time of observation. Tombstones contribute their own knot to
    /// the v2 chain (with `event_type = "erasure"`) per §6.1.1.
    pub is_tombstone: bool,
    /// Wall-clock UNIX time at which this entry was tied into the
    /// shadow chain. Operational metadata; NOT in the §6.1.1 hash.
    pub observed_at_unix: i64,
}

/// Per-string head of the v2 shadow chain.
///
/// Stored under key `string_id` in the heads column family of the
/// shadow store. Used to recover h_{i-1} on the next observation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShadowChainHead {
    /// `event_id` of the latest knot recorded in the v2 chain.
    pub latest_event_id: u64,
    /// `h_i` for that latest knot. The next observed knot chains
    /// over this value.
    pub latest_knot_hash: KnotHash,
    /// Wall-clock UNIX time of the latest update.
    pub updated_at_unix: i64,
}
