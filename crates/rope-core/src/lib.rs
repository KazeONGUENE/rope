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

pub mod types;
pub mod string;
pub mod nucleotide;
pub mod complement;
pub mod lattice;
pub mod clock;
pub mod error;

pub use types::*;
pub use string::*;
pub use nucleotide::*;
pub use complement::*;
pub use lattice::*;
pub use clock::*;
pub use error::*;

/// Prelude module for convenient imports
pub mod prelude {
    pub use crate::types::*;
    pub use crate::string::RopeString;
    pub use crate::nucleotide::Nucleotide;
    pub use crate::complement::Complement;
    pub use crate::lattice::StringLattice;
    pub use crate::clock::LamportClock;
    pub use crate::error::{RopeError, Result};
}

