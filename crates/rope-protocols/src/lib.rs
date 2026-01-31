//! # Advanced Protocols
//!
//! DNA-inspired protocols for the String Lattice:
//! - **Regeneration**: Repair damaged/lost strings
//! - **Erasure**: Controlled deletion (GDPR compliant)
//! - **Gossip**: Gossip-about-gossip communication
//! - **Federation**: Validator set management

pub mod erasure;
pub mod federation;
pub mod gossip;
pub mod regeneration;

// Re-exports
pub use erasure::*;
pub use federation::*;
pub use gossip::{GossipDag, GossipEvent};
pub use regeneration::*;
