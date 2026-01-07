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

pub mod virtual_voting {
    //! Virtual voting mechanism - calculate votes from gossip history
    //! 
    //! Nodes don't explicitly vote. Instead, they derive votes from:
    //! - The order they received strings
    //! - The gossip messages they've seen
    //! - Mathematical determinism from shared history
    
    use std::collections::HashMap;
    
    /// Virtual vote calculated from gossip history
    #[derive(Clone, Debug)]
    pub struct VirtualVote {
        pub round: u64,
        pub voter_id: [u8; 32],
        pub string_id: [u8; 32],
        pub decision: VoteDecision,
    }
    
    /// Vote decision
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum VoteDecision {
        Accept,
        Reject,
        Abstain,
    }
    
    /// Virtual voting state
    pub struct VirtualVotingState {
        round: u64,
        votes: HashMap<[u8; 32], Vec<VirtualVote>>,
    }
    
    impl VirtualVotingState {
        pub fn new() -> Self {
            Self {
                round: 0,
                votes: HashMap::new(),
            }
        }
        
        pub fn current_round(&self) -> u64 {
            self.round
        }
        
        pub fn advance_round(&mut self) {
            self.round += 1;
        }
    }
    
    impl Default for VirtualVotingState {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod finality {
    //! Finality determination
    //! 
    //! A string achieves finality when:
    //! - It's referenced by an anchor string
    //! - The anchor has ≥2/3 validator signatures
    //! - All parent strings are also finalized
    
    /// Finality status of a string
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum FinalityStatus {
        /// Not yet evaluated
        Pending,
        /// Tentatively accepted (may be reordered)
        Tentative { confidence: u8 },
        /// Finalized by anchor string
        Final { anchor_id: [u8; 32] },
        /// Rejected by consensus
        Rejected { reason: String },
    }
    
    impl FinalityStatus {
        pub fn is_final(&self) -> bool {
            matches!(self, FinalityStatus::Final { .. })
        }
        
        pub fn confidence(&self) -> u8 {
            match self {
                FinalityStatus::Pending => 0,
                FinalityStatus::Tentative { confidence } => *confidence,
                FinalityStatus::Final { .. } => 100,
                FinalityStatus::Rejected { .. } => 0,
            }
        }
    }
}

// Re-exports
pub use testimony::*;
pub use anchor::*;
pub use virtual_voting::{VirtualVote, VoteDecision, VirtualVotingState};
pub use finality::FinalityStatus;
