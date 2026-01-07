//! # Federation Generation Protocol
//! 
//! Governs how the L0 Core Federation is formed, maintained, and evolved.
//! The federation consists of permissioned validators responsible for
//! string validation and consensus.

pub mod genesis {
    //! Genesis federation creation
    //! 
    //! Creates the initial federation from a set of founding validators.
    //! The genesis block contains the initial validator set and parameters.
    
    use serde::{Deserialize, Serialize};
    
    /// Genesis validator configuration
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct GenesisValidator {
        pub node_id: [u8; 32],
        pub public_key: Vec<u8>,
        pub name: String,
        pub stake: u64,
    }
    
    /// Genesis configuration
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct GenesisConfig {
        pub chain_id: String,
        pub timestamp: u64,
        pub validators: Vec<GenesisValidator>,
        pub initial_params: FederationParams,
    }
    
    /// Federation parameters set at genesis
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct FederationParams {
        pub min_validators: usize,
        pub max_validators: usize,
        pub block_interval_ms: u64,
        pub testimony_threshold: f64,
        pub anchor_interval: u64,
    }
    
    impl Default for FederationParams {
        fn default() -> Self {
            Self {
                min_validators: 4,
                max_validators: 100,
                block_interval_ms: 1000,
                testimony_threshold: 0.667,
                anchor_interval: 100,
            }
        }
    }
}

pub mod evolution {
    //! Federation membership changes
    //! 
    //! Handles validator additions, removals, and stake changes.
    //! All changes require governance approval.
    
    use serde::{Deserialize, Serialize};
    
    /// Membership change proposal
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub enum MembershipChange {
        AddValidator {
            node_id: [u8; 32],
            public_key: Vec<u8>,
            stake: u64,
        },
        RemoveValidator {
            node_id: [u8; 32],
            reason: String,
        },
        UpdateStake {
            node_id: [u8; 32],
            new_stake: u64,
        },
        UpdateParams {
            new_params: super::genesis::FederationParams,
        },
    }
    
    /// Current federation state
    #[derive(Clone, Debug)]
    pub struct FederationState {
        pub epoch: u64,
        pub validators: Vec<super::genesis::GenesisValidator>,
        pub total_stake: u64,
        pub params: super::genesis::FederationParams,
    }
    
    impl FederationState {
        pub fn quorum_stake(&self) -> u64 {
            (self.total_stake as f64 * self.params.testimony_threshold) as u64
        }
        
        pub fn is_validator(&self, node_id: &[u8; 32]) -> bool {
            self.validators.iter().any(|v| &v.node_id == node_id)
        }
    }
}

pub mod governance {
    //! Proposal and voting mechanisms
    //! 
    //! On-chain governance for federation changes.
    
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;
    
    /// Governance proposal
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Proposal {
        pub id: [u8; 32],
        pub proposer: [u8; 32],
        pub title: String,
        pub description: String,
        pub change: super::evolution::MembershipChange,
        pub created_at: u64,
        pub voting_deadline: u64,
        pub status: ProposalStatus,
    }
    
    /// Proposal status
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ProposalStatus {
        Pending,
        Active,
        Passed,
        Rejected,
        Executed,
        Expired,
    }
    
    /// Vote on a proposal
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Vote {
        pub proposal_id: [u8; 32],
        pub voter_id: [u8; 32],
        pub decision: VoteDecision,
        pub stake: u64,
        pub timestamp: u64,
    }
    
    /// Vote decision
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum VoteDecision {
        Yes,
        No,
        Abstain,
    }
    
    /// Governance state
    pub struct GovernanceState {
        pub proposals: HashMap<[u8; 32], Proposal>,
        pub votes: HashMap<[u8; 32], Vec<Vote>>,
    }
    
    impl GovernanceState {
        pub fn new() -> Self {
            Self {
                proposals: HashMap::new(),
                votes: HashMap::new(),
            }
        }
        
        pub fn add_proposal(&mut self, proposal: Proposal) {
            self.proposals.insert(proposal.id, proposal);
        }
        
        pub fn add_vote(&mut self, vote: Vote) {
            self.votes
                .entry(vote.proposal_id)
                .or_insert_with(Vec::new)
                .push(vote);
        }
        
        pub fn tally_votes(&self, proposal_id: &[u8; 32]) -> (u64, u64, u64) {
            let votes = self.votes.get(proposal_id);
            if votes.is_none() {
                return (0, 0, 0);
            }
            
            let mut yes = 0u64;
            let mut no = 0u64;
            let mut abstain = 0u64;
            
            for vote in votes.unwrap() {
                match vote.decision {
                    VoteDecision::Yes => yes += vote.stake,
                    VoteDecision::No => no += vote.stake,
                    VoteDecision::Abstain => abstain += vote.stake,
                }
            }
            
            (yes, no, abstain)
        }
    }
    
    impl Default for GovernanceState {
        fn default() -> Self {
            Self::new()
        }
    }
}

// Re-exports
pub use genesis::{GenesisConfig, GenesisValidator, FederationParams};
pub use evolution::{MembershipChange, FederationState};
pub use governance::{Proposal, ProposalStatus, Vote, VoteDecision, GovernanceState};
