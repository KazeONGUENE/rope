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
//! 
//! ## Byzantine Fault Tolerance
//! 
//! The protocol tolerates up to f Byzantine validators where n ≥ 3f + 1.
//! For 21 validators, this means up to 6 Byzantine nodes can be tolerated.
//! Finality requires 2f + 1 = 15 testimonies.

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
    use serde::{Deserialize, Serialize};
    
    /// Virtual vote calculated from gossip history
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct VirtualVote {
        pub round: u64,
        pub voter_id: [u8; 32],
        pub string_id: [u8; 32],
        pub decision: VoteDecision,
        pub timestamp: i64,
    }
    
    /// Vote decision
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum VoteDecision {
        Accept,
        Reject,
        Abstain,
    }
    
    /// Virtual voting state for a round
    pub struct VirtualVotingState {
        round: u64,
        votes: HashMap<[u8; 32], Vec<VirtualVote>>,
        famous_witnesses: Vec<[u8; 32]>,
    }
    
    impl VirtualVotingState {
        pub fn new() -> Self {
            Self {
                round: 0,
                votes: HashMap::new(),
                famous_witnesses: Vec::new(),
            }
        }
        
        pub fn current_round(&self) -> u64 {
            self.round
        }
        
        pub fn advance_round(&mut self) {
            self.round += 1;
            self.votes.clear();
        }
        
        /// Record a virtual vote
        pub fn record_vote(&mut self, vote: VirtualVote) {
            self.votes
                .entry(vote.string_id)
                .or_insert_with(Vec::new)
                .push(vote);
        }
        
        /// Get votes for a string
        pub fn get_votes(&self, string_id: &[u8; 32]) -> &[VirtualVote] {
            self.votes.get(string_id).map(|v| v.as_slice()).unwrap_or(&[])
        }
        
        /// Count accept votes for a string
        pub fn count_accepts(&self, string_id: &[u8; 32]) -> usize {
            self.get_votes(string_id)
                .iter()
                .filter(|v| v.decision == VoteDecision::Accept)
                .count()
        }
        
        /// Determine if string has supermajority
        pub fn has_supermajority(&self, string_id: &[u8; 32], total_validators: usize) -> bool {
            let accepts = self.count_accepts(string_id);
            let threshold = (total_validators * 2) / 3 + 1;
            accepts >= threshold
        }
        
        /// Add a famous witness
        pub fn add_famous_witness(&mut self, witness_id: [u8; 32]) {
            if !self.famous_witnesses.contains(&witness_id) {
                self.famous_witnesses.push(witness_id);
            }
        }
        
        /// Get famous witnesses for current round
        pub fn famous_witnesses(&self) -> &[[u8; 32]] {
            &self.famous_witnesses
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
    //! - The anchor has ≥2/3 validator signatures (testimonies)
    //! - All parent strings are also finalized
    
    use serde::{Deserialize, Serialize};
    
    /// Finality status of a string
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
        
        pub fn is_pending(&self) -> bool {
            matches!(self, FinalityStatus::Pending)
        }
        
        pub fn is_rejected(&self) -> bool {
            matches!(self, FinalityStatus::Rejected { .. })
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
    
    impl Default for FinalityStatus {
        fn default() -> Self {
            FinalityStatus::Pending
        }
    }
}

// Re-exports
pub use testimony::{
    Testimony, TestimonySignature, TestimonyMetadata,
    TestimonyCollection, TestimonyCollector, TestimonyConfig,
    FinalityProgress, TestimonyError,
};
pub use anchor::AnchorString;
pub use virtual_voting::{VirtualVote, VoteDecision, VirtualVotingState};
pub use finality::FinalityStatus;
