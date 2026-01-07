//! # Testimony Consensus Protocol
//! 
//! Byzantine fault-tolerant consensus for the String Lattice.
//! Extends hashgraph virtual voting with accountable attestations.
//! 
//! ## Consensus Phases
//! 
//! 1. String Creation - Any authorized node creates strings
//! 2. Gossip Propagation - Strings spread through gossip-about-gossip
//! 3. Virtual Voting - Nodes calculate votes from gossip history
//! 4. Testimony Collection - Explicit attestations for high-assurance
//! 5. Finality Declaration - Anchor strings confirm finality

pub mod testimony;
pub mod anchor;
pub mod virtual_voting;
pub mod finality;

// Re-exports
pub use testimony::*;
pub use anchor::*;

