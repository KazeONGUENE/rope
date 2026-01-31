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
pub mod lattice;
pub mod nucleotide;
pub mod string;
pub mod types;

pub use clock::*;
pub use complement::*;
pub use error::*;
pub use lattice::*;
pub use nucleotide::*;
pub use string::*;
pub use types::*;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::clock::LamportClock;
    pub use crate::complement::Complement;
    pub use crate::error::{Result, RopeError};
    pub use crate::lattice::StringLattice;
    pub use crate::nucleotide::Nucleotide;
    pub use crate::string::RopeString;
    pub use crate::types::*;
}
