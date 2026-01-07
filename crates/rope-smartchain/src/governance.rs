//! # Governance Module
//! 
//! Governs minting and critical operations in Datachain Rope Smartchain.
//! 
//! ## Minting Governance Structure
//! 
//! For DC FAT minting to be approved, the following must ALL approve:
//! 
//! 1. **AI Testimony Agents** (5 required) - Validates the minting request
//! 2. **Random Governors** (5 active wallets) - Selected randomly from active validators
//! 3. **Foundation Members** (2 required) - Datachain Foundation identified wallets
//! 
//! Total: 12 approvals required for minting
//! 
//! ## Security Model
//! 
//! - AI agents prevent fraudulent/invalid minting requests
//! - Random governors prevent collusion (unpredictable selection)
//! - Foundation members provide final oversight and accountability

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use parking_lot::RwLock;

/// Governance configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceConfig {
    /// Number of AI testimony agents required
    pub required_ai_agents: u32,
    
    /// Number of random governors required
    pub required_random_governors: u32,
    
    /// Number of foundation members required
    pub required_foundation_members: u32,
    
    /// Minimum time between governor selections (prevents gaming)
    pub governor_rotation_interval_secs: u64,
    
    /// Voting timeout in seconds
    pub voting_timeout_secs: u64,
}

impl Default for GovernanceConfig {
    fn default() -> Self {
        Self {
            required_ai_agents: 5,
            required_random_governors: 5,
            required_foundation_members: 2,
            governor_rotation_interval_secs: 3600, // 1 hour
            voting_timeout_secs: 86400, // 24 hours
        }
    }
}

/// Foundation member wallet
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct FoundationMember {
    /// Wallet address (NodeId)
    pub wallet: [u8; 32],
    
    /// Member name/identifier
    pub name: String,
    
    /// Role in foundation
    pub role: FoundationRole,
    
    /// Is member currently active
    pub is_active: bool,
}

/// Foundation member roles
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Hash)]
pub enum FoundationRole {
    /// CEO/Executive
    Executive,
    /// Board member
    Board,
    /// Technical lead
    Technical,
    /// Legal/Compliance
    Legal,
    /// Treasury
    Treasury,
}

/// Governor selection result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernorSelection {
    /// Selected random governors (5 wallets)
    pub random_governors: Vec<[u8; 32]>,
    
    /// Required foundation members (2 wallets)
    pub foundation_members: Vec<[u8; 32]>,
    
    /// Selection timestamp
    pub selected_at: i64,
    
    /// Selection proof (for verifiability)
    pub selection_proof: [u8; 32],
    
    /// Expiry timestamp
    pub expires_at: i64,
}

/// Minting proposal requiring governance approval
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MintingProposal {
    /// Proposal ID
    pub id: [u8; 32],
    
    /// Token to mint
    pub token_id: [u8; 32],
    
    /// Amount to mint
    pub amount: u128,
    
    /// Recipient wallet
    pub recipient: [u8; 32],
    
    /// Reason for minting
    pub reason: String,
    
    /// Proposer wallet
    pub proposer: [u8; 32],
    
    /// Created timestamp
    pub created_at: i64,
    
    /// Current status
    pub status: ProposalStatus,
    
    /// AI agent approvals
    pub ai_approvals: Vec<AIApproval>,
    
    /// Governor approvals
    pub governor_approvals: Vec<GovernorApproval>,
    
    /// Foundation approvals
    pub foundation_approvals: Vec<FoundationApproval>,
    
    /// Selected governors for this proposal
    pub governor_selection: GovernorSelection,
}

/// Proposal status
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProposalStatus {
    /// Awaiting AI validation
    PendingAI,
    /// Awaiting governor votes
    PendingGovernors,
    /// Awaiting foundation approval
    PendingFoundation,
    /// All approvals received
    Approved,
    /// Minting executed
    Executed { tx_id: [u8; 32] },
    /// Rejected
    Rejected { reason: String },
    /// Expired
    Expired,
}

/// AI agent approval
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AIApproval {
    pub agent_id: [u8; 32],
    pub agent_type: String,
    pub approved: bool,
    pub confidence: f64,
    pub reasoning: String,
    pub timestamp: i64,
    pub signature: Vec<u8>,
}

/// Governor approval
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernorApproval {
    pub governor_wallet: [u8; 32],
    pub approved: bool,
    pub comment: Option<String>,
    pub timestamp: i64,
    pub signature: Vec<u8>,
}

/// Foundation member approval
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FoundationApproval {
    pub member_wallet: [u8; 32],
    pub member_name: String,
    pub approved: bool,
    pub comment: Option<String>,
    pub timestamp: i64,
    pub signature: Vec<u8>,
}

/// Governance state manager
pub struct MintingGovernance {
    /// Configuration
    config: GovernanceConfig,
    
    /// Registered foundation members
    foundation_members: RwLock<Vec<FoundationMember>>,
    
    /// Active validator wallets (pool for random selection)
    active_validators: RwLock<HashSet<[u8; 32]>>,
    
    /// Pending proposals
    pending_proposals: RwLock<HashMap<[u8; 32], MintingProposal>>,
    
    /// Completed proposals (for audit)
    completed_proposals: RwLock<Vec<MintingProposal>>,
    
    /// Last governor selection
    last_selection: RwLock<Option<GovernorSelection>>,
}

impl MintingGovernance {
    /// Create new governance with default config
    pub fn new() -> Self {
        Self {
            config: GovernanceConfig::default(),
            foundation_members: RwLock::new(Vec::new()),
            active_validators: RwLock::new(HashSet::new()),
            pending_proposals: RwLock::new(HashMap::new()),
            completed_proposals: RwLock::new(Vec::new()),
            last_selection: RwLock::new(None),
        }
    }
    
    /// Create with custom config
    pub fn with_config(config: GovernanceConfig) -> Self {
        Self {
            config,
            ..Self::new()
        }
    }
    
    /// Register a foundation member
    pub fn register_foundation_member(&self, member: FoundationMember) {
        self.foundation_members.write().push(member);
    }
    
    /// Register an active validator wallet
    pub fn register_validator(&self, wallet: [u8; 32]) {
        self.active_validators.write().insert(wallet);
    }
    
    /// Remove a validator
    pub fn remove_validator(&self, wallet: &[u8; 32]) {
        self.active_validators.write().remove(wallet);
    }
    
    /// Select random governors for a new proposal
    pub fn select_governors(&self, entropy: &[u8; 32]) -> Result<GovernorSelection, GovernanceError> {
        let validators = self.active_validators.read();
        let foundation = self.foundation_members.read();
        
        // Need enough validators
        if validators.len() < self.config.required_random_governors as usize {
            return Err(GovernanceError::InsufficientValidators {
                required: self.config.required_random_governors as usize,
                available: validators.len(),
            });
        }
        
        // Need enough active foundation members
        let active_foundation: Vec<_> = foundation.iter()
            .filter(|m| m.is_active)
            .collect();
        
        if active_foundation.len() < self.config.required_foundation_members as usize {
            return Err(GovernanceError::InsufficientFoundationMembers {
                required: self.config.required_foundation_members as usize,
                available: active_foundation.len(),
            });
        }
        
        // Random selection using entropy (deterministic for verifiability)
        let mut selected_governors = Vec::new();
        let validator_list: Vec<_> = validators.iter().cloned().collect();
        
        // Use entropy to select random indices
        let mut selection_state = *entropy;
        for i in 0..self.config.required_random_governors {
            selection_state = *blake3::hash(&[
                &selection_state[..],
                &i.to_le_bytes(),
            ].concat()).as_bytes();
            
            let index = u64::from_le_bytes(selection_state[0..8].try_into().unwrap()) as usize 
                % validator_list.len();
            
            // Avoid duplicates
            let mut candidate = validator_list[index];
            let mut attempts = 0;
            while selected_governors.contains(&candidate) && attempts < 100 {
                selection_state = *blake3::hash(&selection_state).as_bytes();
                let new_index = u64::from_le_bytes(selection_state[0..8].try_into().unwrap()) as usize 
                    % validator_list.len();
                candidate = validator_list[new_index];
                attempts += 1;
            }
            
            selected_governors.push(candidate);
        }
        
        // Select foundation members (first N active members)
        let foundation_wallets: Vec<_> = active_foundation.iter()
            .take(self.config.required_foundation_members as usize)
            .map(|m| m.wallet)
            .collect();
        
        let now = chrono::Utc::now().timestamp();
        let selection_proof = *blake3::hash(&[
            entropy.as_slice(),
            &now.to_le_bytes(),
        ].concat()).as_bytes();
        
        let selection = GovernorSelection {
            random_governors: selected_governors,
            foundation_members: foundation_wallets,
            selected_at: now,
            selection_proof,
            expires_at: now + self.config.voting_timeout_secs as i64,
        };
        
        *self.last_selection.write() = Some(selection.clone());
        
        Ok(selection)
    }
    
    /// Create a minting proposal
    pub fn create_proposal(
        &self,
        token_id: [u8; 32],
        amount: u128,
        recipient: [u8; 32],
        reason: String,
        proposer: [u8; 32],
        entropy: &[u8; 32],
    ) -> Result<MintingProposal, GovernanceError> {
        // Select governors
        let governor_selection = self.select_governors(entropy)?;
        
        let now = chrono::Utc::now().timestamp();
        let proposal_id = *blake3::hash(&[
            &token_id[..],
            &amount.to_le_bytes(),
            &recipient[..],
            &now.to_le_bytes(),
        ].concat()).as_bytes();
        
        let proposal = MintingProposal {
            id: proposal_id,
            token_id,
            amount,
            recipient,
            reason,
            proposer,
            created_at: now,
            status: ProposalStatus::PendingAI,
            ai_approvals: Vec::new(),
            governor_approvals: Vec::new(),
            foundation_approvals: Vec::new(),
            governor_selection,
        };
        
        self.pending_proposals.write().insert(proposal_id, proposal.clone());
        
        Ok(proposal)
    }
    
    /// Submit AI agent approval
    pub fn submit_ai_approval(
        &self,
        proposal_id: &[u8; 32],
        approval: AIApproval,
    ) -> Result<(), GovernanceError> {
        let mut proposals = self.pending_proposals.write();
        let proposal = proposals.get_mut(proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        
        // Check not already approved by this agent
        if proposal.ai_approvals.iter().any(|a| a.agent_id == approval.agent_id) {
            return Err(GovernanceError::AlreadyVoted);
        }
        
        proposal.ai_approvals.push(approval);
        
        // Check if enough AI approvals
        let approved_count = proposal.ai_approvals.iter()
            .filter(|a| a.approved)
            .count();
        
        if approved_count >= self.config.required_ai_agents as usize {
            proposal.status = ProposalStatus::PendingGovernors;
        }
        
        Ok(())
    }
    
    /// Submit governor approval
    pub fn submit_governor_approval(
        &self,
        proposal_id: &[u8; 32],
        approval: GovernorApproval,
    ) -> Result<(), GovernanceError> {
        let mut proposals = self.pending_proposals.write();
        let proposal = proposals.get_mut(proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        
        // Check governor is in selection
        if !proposal.governor_selection.random_governors.contains(&approval.governor_wallet) {
            return Err(GovernanceError::NotAuthorized);
        }
        
        // Check not already voted
        if proposal.governor_approvals.iter().any(|a| a.governor_wallet == approval.governor_wallet) {
            return Err(GovernanceError::AlreadyVoted);
        }
        
        proposal.governor_approvals.push(approval);
        
        // Check if enough governor approvals
        let approved_count = proposal.governor_approvals.iter()
            .filter(|a| a.approved)
            .count();
        
        if approved_count >= self.config.required_random_governors as usize {
            proposal.status = ProposalStatus::PendingFoundation;
        }
        
        Ok(())
    }
    
    /// Submit foundation member approval
    pub fn submit_foundation_approval(
        &self,
        proposal_id: &[u8; 32],
        approval: FoundationApproval,
    ) -> Result<(), GovernanceError> {
        let mut proposals = self.pending_proposals.write();
        let proposal = proposals.get_mut(proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        
        // Check member is in required list
        if !proposal.governor_selection.foundation_members.contains(&approval.member_wallet) {
            return Err(GovernanceError::NotAuthorized);
        }
        
        // Check not already voted
        if proposal.foundation_approvals.iter().any(|a| a.member_wallet == approval.member_wallet) {
            return Err(GovernanceError::AlreadyVoted);
        }
        
        proposal.foundation_approvals.push(approval);
        
        // Check if enough foundation approvals
        let approved_count = proposal.foundation_approvals.iter()
            .filter(|a| a.approved)
            .count();
        
        if approved_count >= self.config.required_foundation_members as usize {
            proposal.status = ProposalStatus::Approved;
        }
        
        Ok(())
    }
    
    /// Check if proposal is fully approved and ready for execution
    pub fn is_approved(&self, proposal_id: &[u8; 32]) -> bool {
        self.pending_proposals.read()
            .get(proposal_id)
            .map(|p| p.status == ProposalStatus::Approved)
            .unwrap_or(false)
    }
    
    /// Get proposal details
    pub fn get_proposal(&self, proposal_id: &[u8; 32]) -> Option<MintingProposal> {
        self.pending_proposals.read().get(proposal_id).cloned()
    }
    
    /// Mark proposal as executed
    pub fn mark_executed(&self, proposal_id: &[u8; 32], tx_id: [u8; 32]) -> Result<(), GovernanceError> {
        let mut proposals = self.pending_proposals.write();
        let proposal = proposals.get_mut(proposal_id)
            .ok_or(GovernanceError::ProposalNotFound)?;
        
        if proposal.status != ProposalStatus::Approved {
            return Err(GovernanceError::NotApproved);
        }
        
        proposal.status = ProposalStatus::Executed { tx_id };
        
        // Move to completed
        let completed = proposals.remove(proposal_id).unwrap();
        self.completed_proposals.write().push(completed);
        
        Ok(())
    }
    
    /// Get governance requirements summary
    pub fn requirements(&self) -> GovernanceRequirements {
        GovernanceRequirements {
            ai_agents: self.config.required_ai_agents,
            random_governors: self.config.required_random_governors,
            foundation_members: self.config.required_foundation_members,
            total_required: self.config.required_ai_agents 
                + self.config.required_random_governors 
                + self.config.required_foundation_members,
        }
    }
}

impl Default for MintingGovernance {
    fn default() -> Self {
        Self::new()
    }
}

/// Summary of governance requirements
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct GovernanceRequirements {
    pub ai_agents: u32,
    pub random_governors: u32,
    pub foundation_members: u32,
    pub total_required: u32,
}

/// Governance errors
#[derive(Clone, Debug)]
pub enum GovernanceError {
    ProposalNotFound,
    AlreadyVoted,
    NotAuthorized,
    NotApproved,
    Expired,
    InsufficientValidators { required: usize, available: usize },
    InsufficientFoundationMembers { required: usize, available: usize },
}

impl std::fmt::Display for GovernanceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GovernanceError::ProposalNotFound => write!(f, "Proposal not found"),
            GovernanceError::AlreadyVoted => write!(f, "Already voted on this proposal"),
            GovernanceError::NotAuthorized => write!(f, "Not authorized to vote"),
            GovernanceError::NotApproved => write!(f, "Proposal not approved"),
            GovernanceError::Expired => write!(f, "Proposal expired"),
            GovernanceError::InsufficientValidators { required, available } => {
                write!(f, "Insufficient validators: {} required, {} available", required, available)
            }
            GovernanceError::InsufficientFoundationMembers { required, available } => {
                write!(f, "Insufficient foundation members: {} required, {} available", required, available)
            }
        }
    }
}

impl std::error::Error for GovernanceError {}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_governance_requirements() {
        let governance = MintingGovernance::new();
        let req = governance.requirements();
        
        assert_eq!(req.ai_agents, 5);
        assert_eq!(req.random_governors, 5);
        assert_eq!(req.foundation_members, 2);
        assert_eq!(req.total_required, 12);
    }
    
    #[test]
    fn test_register_foundation_member() {
        let governance = MintingGovernance::new();
        
        governance.register_foundation_member(FoundationMember {
            wallet: [1u8; 32],
            name: "CEO".to_string(),
            role: FoundationRole::Executive,
            is_active: true,
        });
        
        governance.register_foundation_member(FoundationMember {
            wallet: [2u8; 32],
            name: "CTO".to_string(),
            role: FoundationRole::Technical,
            is_active: true,
        });
        
        assert_eq!(governance.foundation_members.read().len(), 2);
    }
    
    #[test]
    fn test_governor_selection() {
        let governance = MintingGovernance::new();
        
        // Register 10 validators
        for i in 0..10 {
            governance.register_validator([i as u8; 32]);
        }
        
        // Register 2 foundation members
        governance.register_foundation_member(FoundationMember {
            wallet: [100u8; 32],
            name: "CEO".to_string(),
            role: FoundationRole::Executive,
            is_active: true,
        });
        governance.register_foundation_member(FoundationMember {
            wallet: [101u8; 32],
            name: "CTO".to_string(),
            role: FoundationRole::Technical,
            is_active: true,
        });
        
        // Select governors
        let entropy = [42u8; 32];
        let selection = governance.select_governors(&entropy).unwrap();
        
        assert_eq!(selection.random_governors.len(), 5);
        assert_eq!(selection.foundation_members.len(), 2);
    }
    
    #[test]
    fn test_full_approval_flow() {
        let governance = MintingGovernance::new();
        
        // Setup
        for i in 0..10 {
            governance.register_validator([i as u8; 32]);
        }
        governance.register_foundation_member(FoundationMember {
            wallet: [100u8; 32],
            name: "CEO".to_string(),
            role: FoundationRole::Executive,
            is_active: true,
        });
        governance.register_foundation_member(FoundationMember {
            wallet: [101u8; 32],
            name: "CTO".to_string(),
            role: FoundationRole::Technical,
            is_active: true,
        });
        
        // Create proposal
        let proposal = governance.create_proposal(
            [0u8; 32], // token_id
            1000,
            [50u8; 32], // recipient
            "Test minting".to_string(),
            [99u8; 32], // proposer
            &[42u8; 32], // entropy
        ).unwrap();
        
        // Submit 5 AI approvals
        for i in 0..5 {
            governance.submit_ai_approval(&proposal.id, AIApproval {
                agent_id: [i as u8; 32],
                agent_type: "ValidationAgent".to_string(),
                approved: true,
                confidence: 0.95,
                reasoning: "Valid minting request".to_string(),
                timestamp: chrono::Utc::now().timestamp(),
                signature: Vec::new(),
            }).unwrap();
        }
        
        // Check status moved to PendingGovernors
        let updated = governance.get_proposal(&proposal.id).unwrap();
        assert_eq!(updated.status, ProposalStatus::PendingGovernors);
        
        // Submit 5 governor approvals
        for wallet in &updated.governor_selection.random_governors {
            governance.submit_governor_approval(&proposal.id, GovernorApproval {
                governor_wallet: *wallet,
                approved: true,
                comment: None,
                timestamp: chrono::Utc::now().timestamp(),
                signature: Vec::new(),
            }).unwrap();
        }
        
        // Check status moved to PendingFoundation
        let updated = governance.get_proposal(&proposal.id).unwrap();
        assert_eq!(updated.status, ProposalStatus::PendingFoundation);
        
        // Submit 2 foundation approvals
        for wallet in &updated.governor_selection.foundation_members {
            governance.submit_foundation_approval(&proposal.id, FoundationApproval {
                member_wallet: *wallet,
                member_name: "Member".to_string(),
                approved: true,
                comment: None,
                timestamp: chrono::Utc::now().timestamp(),
                signature: Vec::new(),
            }).unwrap();
        }
        
        // Should be approved now
        assert!(governance.is_approved(&proposal.id));
        
        let final_proposal = governance.get_proposal(&proposal.id).unwrap();
        assert_eq!(final_proposal.status, ProposalStatus::Approved);
    }
}

