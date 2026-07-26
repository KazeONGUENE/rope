//! # Datachain Rope Core
//!
//! Core data structures for the Datachain Rope distributed information communication protocol.
//!
//! This crate provides the fundamental building blocks:
//! - `String` - The fundamental unit of information (analogous to DNA strand)
//! - `Nucleotide` - Individual information unit within a string
//! - `Complement` - Verification string for integrity and regeneration
//! - `StringLattice` - The core DAG structure replacing blockchain
//!
//! ## Architecture
//!
//! Unlike blockchain's linear chain, Datachain Rope organizes data into strings
//! that interweave to form a resilient, regenerative structure - the Rope.
//!
//! ```text
//!          ┌─────────────────────────────────────────┐
//!          │           STRING LATTICE (DAG)          │
//!          │                                         │
//!          │   S₁ ──┬──► S₃ ──┬──► S₅ (anchor)      │
//!          │        │        │                       │
//!          │   S₂ ──┘        └──► S₆               │
//!          │        ╲              ╲                 │
//!          │   S̄₁ ──┴──► S̄₃ ──┴──► S̄₅ (complement) │
//!          │                                         │
//!          └─────────────────────────────────────────┘
//! ```

pub mod clock;
pub mod complement;
pub mod error;
pub mod knot_dag;
pub mod knot_hash;
pub mod lattice;
pub mod nucleotide;
pub mod personal_ledger;
pub mod string;
pub mod types;

// `clock::*` and `lattice::*` both export a `pub const NUM_SHARDS: usize = 256;`.
// They are independent constants serving different subsystems (clock-shard
// count for the per-shard Hybrid Logical Clock vs lattice-shard count for
// the StringLattice wallet-bucket map). They share value 256 today by
// convention but may diverge in the future. To avoid the
// `ambiguous_glob_reexports` warning while preserving the historical
// crate-root re-exports of both, we glob-export `clock` first (so the
// resolver can pick a deterministic NUM_SHARDS) and explicitly alias both
// constants under unambiguous names. Consumers should prefer the aliases.
pub use clock::NUM_SHARDS as CLOCK_NUM_SHARDS;
pub use knot_dag::{KnotDag, KnotDagError, KnotDagRegistry, KnotDagSnapshot, KNOT_DAG_NUM_SHARDS};
pub use lattice::NUM_SHARDS as LATTICE_NUM_SHARDS;

#[allow(ambiguous_glob_reexports)]
pub use clock::*;
pub use complement::*;
pub use error::*;
pub use knot_hash::{
    compute_event_metadata_hash, compute_knot_hash, tombstone_preimage, EventMetadata,
    EventMetadataHash, KnotHash, KnotHashPreImage, WitnessId, EVENT_METADATA_HASH_TAG,
    KNOT_HASH_CHAIN_TAG,
};
#[allow(ambiguous_glob_reexports)]
pub use lattice::*;
pub use nucleotide::*;
pub use personal_ledger::*;
pub use string::*;
pub use types::*;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::clock::LamportClock;
    pub use crate::complement::Complement;
    pub use crate::error::{Result, RopeError};
    pub use crate::knot_hash::{
        compute_event_metadata_hash, compute_knot_hash, tombstone_preimage, EventMetadata,
        EventMetadataHash, KnotHash, KnotHashPreImage, WitnessId, EVENT_METADATA_HASH_TAG,
        KNOT_HASH_CHAIN_TAG,
    };
    pub use crate::lattice::StringLattice;
    pub use crate::nucleotide::Nucleotide;
    pub use crate::personal_ledger::{
        EntryPieceMap, InteractionRecord, InteractionType, LedgerChain, LedgerDescriptor,
        StringKind, StringRegistry,
    };
    // Backward-compat alias for v1.0/1.1 callers.
    #[allow(deprecated)]
    pub use crate::personal_ledger::LedgerRegistry;
    pub use crate::string::RopeString;
    pub use crate::types::*;
}
