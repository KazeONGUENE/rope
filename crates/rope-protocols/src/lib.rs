//! # Advanced Protocols
//! 
//! DNA-inspired protocols for the String Lattice:
//! - **Regeneration**: Repair damaged/lost strings
//! - **Erasure**: Controlled deletion (GDPR compliant)
//! - **Gossip**: Gossip-about-gossip communication
//! - **Federation**: Validator set management

pub mod regeneration;
pub mod erasure;
pub mod gossip;
pub mod federation;

// Re-exports
pub use regeneration::*;
pub use erasure::*;
pub use gossip::{GossipEvent, GossipDag};
pub use federation::*;
