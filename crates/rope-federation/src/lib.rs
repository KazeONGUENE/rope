//! # Federation Generation Protocol
//! 
//! Governs how Federations and Communities are formed, maintained, and evolved.
//! Based on the 2018 Federation Generation Schema:
//! 
//! CREATE -> GENERATE -> Federation/Community -> Banking/Global -> Protocols -> 
//! Identity (KYC/AML) -> Predictability -> Wallet/Consensus
//! 
//! ## Architecture
//! 
//! ```text
//! +--------+     +----------+     +------------+     +----------+
//! | CREATE | --> | GENERATE | --> | Federation | --> | Protocols|
//! +--------+     +----------+     +------------+     +----------+
//!                                        |                 |
//!                                        v                 v
//!                                 +------------+     +----------+
//!                                 | Community  | --> | Identity |
//!                                 +------------+     +----------+
//!                                        |                 |
//!                                        v                 v
//!                                 +------------+     +--------+
//!                                 | DataWallet | <-- | Wallet |
//!                                 +------------+     +--------+
//! ```

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// Re-exports from inline modules (defined below)
pub use genesis::{GenesisConfig, GenesisValidator, FederationParams};
pub use evolution::{MembershipChange, FederationState};
pub use governance::{Proposal, ProposalStatus, Vote, VoteDecision, GovernanceState};
pub use community::{Community, CommunityConfig, CommunityType};
pub use project::{ProjectSubmission, ProjectStatus, ProjectCategory};

// =============================================================================
// Genesis Module - Federation Creation
// =============================================================================

pub mod genesis {
    //! Genesis federation creation
    //! 
    //! Creates the initial federation from configuration.
    //! The genesis block contains the initial validator set and parameters.
    
    use super::*;
    
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

// =============================================================================
// Evolution Module - Membership Changes
// =============================================================================

pub mod evolution {
    //! Federation membership changes
    //! 
    //! Handles validator additions, removals, and stake changes.
    //! All changes require governance approval.
    
    use super::*;
    
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

// =============================================================================
// Governance Module - Voting & Proposals
// =============================================================================

pub mod governance {
    //! Proposal and voting mechanisms
    //! 
    //! On-chain governance for federation changes.
    
    use super::*;
    
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

// =============================================================================
// Community Module - Community Generation
// =============================================================================

pub mod community {
    //! Community Generation following the 2018 schema
    //! 
    //! Communities are created within Federations with:
    //! - DataWallet generation (10,000,000 per community)
    //! - Protocol invocations (Native DC, Hyperledger, NXT, EOS, etc.)
    //! - KYC/AML compliance
    //! - Predictability AI features
    //! - Crypto currency support
    
    use super::*;
    
    /// Community type from schema
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum CommunityType {
        /// Structured communities: City, Object, Contributors
        Structured,
        /// Unstructured communities: Fans, Artists, Musicians
        Unstructured,
        /// Autonomous communities: AI, Expert Systems, Bots
        Autonomous,
    }
    
    /// Community scope
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum CommunityScope {
        Global,
        Regional,
        Local,
    }
    
    /// Industry sector
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum IndustrySector {
        Banking,
        Healthcare,
        Automotive,
        Mobility,
        Hospitality,
        HumanRights,
        Energy,
        Agricultural,
        PublicInstitution,
        Technology,
        Entertainment,
        Education,
        Retail,
        Logistics,
        Manufacturing,
    }
    
    /// Protocol types available for invocation
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum Protocol {
        // Native protocols
        NativeDC,
        Hyperledger,
        NXT,
        EOS,
        Wanchain,
        Lisk,
        Ethereum,
        Blockchain,
        Tangle,
        Hashgraph,
        Gnutella,
        GSM,
        Bittorrent,
        // Custom
        Custom(String),
    }
    
    /// Identity/Compliance protocols
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum IdentityProtocol {
        ECytizenship,
        ISO_IEC_24760_1,  // ISO/IEC 24760-1
        EPassport,
        SWIFT,
        SEPA,
        Custom(String),
    }
    
    /// Predictability/AI features
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum PredictabilityFeature {
        Adaptability,
        Matching,
        Retracement,
        ContractMining,
        RiskManagement,
        FraudDetection,
        Scoring,
        Custom(String),
    }
    
    /// Supported cryptocurrencies
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum CryptoCurrency {
        DC,
        Bitcoin,
        ETH,
        EOS,
        WAN,
        Custom(String),
    }
    
    /// Community configuration
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct CommunityConfig {
        /// Community type (Structured, Unstructured, Autonomous)
        pub community_type: CommunityType,
        
        /// Scope (Global, Regional, Local)
        pub scope: CommunityScope,
        
        /// Industry sector
        pub industry: IndustrySector,
        
        /// Data wallets to generate (default: 10,000,000 per schema)
        pub data_wallets_count: u64,
        
        /// Individual chains to generate (default: 10,000,000 per schema)
        pub individual_chains_count: u64,
        
        /// Native protocols to invoke
        pub native_protocols: Vec<Protocol>,
        
        /// External protocols to invoke
        pub external_protocols: Vec<Protocol>,
        
        /// Identity/compliance protocols
        pub identity_protocols: Vec<IdentityProtocol>,
        
        /// KYC/AML enabled
        pub kyc_aml_enabled: bool,
        
        /// Predictability AI features
        pub predictability_features: Vec<PredictabilityFeature>,
        
        /// Supported cryptocurrencies
        pub crypto_currencies: Vec<CryptoCurrency>,
        
        /// Consensus type (default: PoA)
        pub consensus_type: String,
        
        /// Web services enabled
        pub web_services_enabled: bool,
    }
    
    impl Default for CommunityConfig {
        fn default() -> Self {
            Self {
                community_type: CommunityType::Structured,
                scope: CommunityScope::Regional,
                industry: IndustrySector::Technology,
                data_wallets_count: 10_000_000,
                individual_chains_count: 10_000_000,
                native_protocols: vec![Protocol::NativeDC],
                external_protocols: vec![],
                identity_protocols: vec![
                    IdentityProtocol::EPassport,
                    IdentityProtocol::ISO_IEC_24760_1,
                ],
                kyc_aml_enabled: true,
                predictability_features: vec![
                    PredictabilityFeature::Adaptability,
                    PredictabilityFeature::Matching,
                    PredictabilityFeature::Retracement,
                    PredictabilityFeature::ContractMining,
                    PredictabilityFeature::RiskManagement,
                    PredictabilityFeature::FraudDetection,
                    PredictabilityFeature::Scoring,
                ],
                crypto_currencies: vec![
                    CryptoCurrency::DC,
                    CryptoCurrency::Bitcoin,
                    CryptoCurrency::ETH,
                    CryptoCurrency::EOS,
                    CryptoCurrency::WAN,
                ],
                consensus_type: "PoA".to_string(),
                web_services_enabled: true,
            }
        }
    }
    
    /// Community instance
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Community {
        pub id: [u8; 32],
        pub name: String,
        pub description: String,
        pub federation_id: Option<[u8; 32]>,
        pub creator_id: [u8; 32],
        pub config: CommunityConfig,
        
        /// Instance URL (from schema: "Connect to instance configuration web panel")
        pub instance_url: Option<String>,
        
        /// Genesis entry in Datachain main net
        pub genesis_entry: Option<[u8; 32]>,
        
        /// Wallets generated so far
        pub wallets_generated: u64,
        
        /// Chains generated so far
        pub chains_generated: u64,
        
        pub status: CommunityStatus,
        pub created_at: u64,
        pub activated_at: Option<u64>,
    }
    
    /// Community status
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum CommunityStatus {
        PendingVote,
        Voting,
        Active,
        Suspended,
        Archived,
    }
    
    impl Community {
        /// Create a new community
        pub fn new(
            name: String,
            description: String,
            creator_id: [u8; 32],
            config: CommunityConfig,
        ) -> Self {
            let mut id = [0u8; 32];
            // Generate ID from name + timestamp
            let hash_input = format!("{}:{}", name, chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0));
            let hash = blake3::hash(hash_input.as_bytes());
            id.copy_from_slice(hash.as_bytes());
            
            Self {
                id,
                name,
                description,
                federation_id: None,
                creator_id,
                config,
                instance_url: None,
                genesis_entry: None,
                wallets_generated: 0,
                chains_generated: 0,
                status: CommunityStatus::PendingVote,
                created_at: chrono::Utc::now().timestamp() as u64,
                activated_at: None,
            }
        }
        
        /// Generate DataWallets (batch)
        pub fn generate_wallets(&mut self, count: u64) -> Vec<DataWallet> {
            let mut wallets = Vec::with_capacity(count as usize);
            
            for i in 0..count {
                if self.wallets_generated >= self.config.data_wallets_count {
                    break;
                }
                
                let wallet = DataWallet::generate(
                    self.id,
                    self.wallets_generated + i,
                );
                wallets.push(wallet);
                self.wallets_generated += 1;
            }
            
            wallets
        }
        
        /// Check if community needs community vote
        pub fn requires_vote(&self) -> bool {
            matches!(self.status, CommunityStatus::PendingVote | CommunityStatus::Voting)
        }
        
        /// Activate community after successful vote
        pub fn activate(&mut self) {
            self.status = CommunityStatus::Active;
            self.activated_at = Some(chrono::Utc::now().timestamp() as u64);
        }
    }
    
    /// DataWallet generated for federation/community
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct DataWallet {
        pub id: [u8; 32],
        pub community_id: [u8; 32],
        pub index: u64,
        pub address: [u8; 32],
        pub public_key_ed25519: Option<Vec<u8>>,
        pub public_key_dilithium: Option<Vec<u8>>,
        pub is_activated: bool,
        pub created_at: u64,
    }
    
    impl DataWallet {
        pub fn generate(community_id: [u8; 32], index: u64) -> Self {
            let mut id = [0u8; 32];
            let mut address = [0u8; 32];
            
            // Generate deterministic ID and address
            let id_input = format!("wallet:{}:{}", hex::encode(community_id), index);
            let id_hash = blake3::hash(id_input.as_bytes());
            id.copy_from_slice(id_hash.as_bytes());
            
            let addr_input = format!("addr:{}:{}", hex::encode(community_id), index);
            let addr_hash = blake3::hash(addr_input.as_bytes());
            address.copy_from_slice(addr_hash.as_bytes());
            
            Self {
                id,
                community_id,
                index,
                address,
                public_key_ed25519: None,
                public_key_dilithium: None,
                is_activated: false,
                created_at: chrono::Utc::now().timestamp() as u64,
            }
        }
    }
}

// =============================================================================
// Project Module - Project Submissions
// =============================================================================

pub mod project {
    //! Project submission system
    //! 
    //! "Start Building" submissions that require community vote.
    //! Project owners (individuals, businesses, institutions) submit projects
    //! for validation by DC FAT holders.
    
    use super::*;
    
    /// Project category
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ProjectCategory {
        DeFi,
        NFT,
        Gaming,
        Social,
        Infrastructure,
        DAO,
        Marketplace,
        Identity,
        SupplyChain,
        Healthcare,
        IoT,
        AI_ML,
        Oracle,
        Bridge,
        Other(String),
    }
    
    /// Project development stage
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ProjectStage {
        Idea,
        Prototype,
        MVP,
        Beta,
        Production,
    }
    
    /// Project status
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ProjectStatus {
        PendingReview,
        Voting,
        Approved,
        Rejected,
        Building,
        Launched,
    }
    
    /// Organization type
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum OrganizationType {
        Individual,
        Business,
        Institution,
    }
    
    /// Team member
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct TeamMember {
        pub name: String,
        pub role: String,
        pub linkedin_url: Option<String>,
        pub github_url: Option<String>,
    }
    
    /// Project milestone
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Milestone {
        pub title: String,
        pub description: String,
        pub target_date: Option<String>,
        pub is_completed: bool,
    }
    
    /// Feature specification
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct Feature {
        pub name: String,
        pub description: String,
        pub priority: String,  // high, medium, low
    }
    
    /// Project submission
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct ProjectSubmission {
        pub id: [u8; 32],
        
        // Project info
        pub name: String,
        pub tagline: Option<String>,
        pub description: String,
        pub category: ProjectCategory,
        pub stage: ProjectStage,
        
        // Submitter info
        pub submitter_id: [u8; 32],
        pub submitter_name: Option<String>,
        pub submitter_email: Option<String>,
        pub organization_name: Option<String>,
        pub organization_type: OrganizationType,
        
        // Technical specs
        pub tech_stack: Vec<String>,
        pub architecture_description: Option<String>,
        
        // Functional specs
        pub features: Vec<Feature>,
        pub use_cases: Option<String>,
        pub target_users: Option<String>,
        
        // Protocol integration
        pub required_protocols: Vec<community::Protocol>,
        pub external_integrations: Vec<String>,
        
        // AI requirements
        pub requires_ai_testimony: bool,
        pub ai_agent_requirements: Option<String>,
        
        // Documentation
        pub whitepaper_url: Option<String>,
        pub documentation_url: Option<String>,
        pub github_url: Option<String>,
        pub website_url: Option<String>,
        pub demo_url: Option<String>,
        
        // Team
        pub team_members: Vec<TeamMember>,
        
        // Milestones
        pub milestones: Vec<Milestone>,
        
        // Funding
        pub funding_requested: u64,
        pub funding_currency: String,
        pub funding_breakdown: Option<String>,
        
        // Voting
        pub status: ProjectStatus,
        pub voting_starts_at: Option<u64>,
        pub voting_ends_at: Option<u64>,
        pub vote_count_for: u64,
        pub vote_count_against: u64,
        pub required_votes: u64,
        pub approval_threshold: f64,
        
        // Timestamps
        pub created_at: u64,
        pub updated_at: u64,
        pub approved_at: Option<u64>,
        pub launched_at: Option<u64>,
    }
    
    impl ProjectSubmission {
        /// Create new project submission
        pub fn new(
            name: String,
            description: String,
            category: ProjectCategory,
            submitter_id: [u8; 32],
            organization_type: OrganizationType,
        ) -> Self {
            let mut id = [0u8; 32];
            let hash_input = format!("project:{}:{}", name, chrono::Utc::now().timestamp_nanos_opt().unwrap_or(0));
            let hash = blake3::hash(hash_input.as_bytes());
            id.copy_from_slice(hash.as_bytes());
            
            let now = chrono::Utc::now().timestamp() as u64;
            
            Self {
                id,
                name,
                tagline: None,
                description,
                category,
                stage: ProjectStage::Idea,
                submitter_id,
                submitter_name: None,
                submitter_email: None,
                organization_name: None,
                organization_type,
                tech_stack: vec![],
                architecture_description: None,
                features: vec![],
                use_cases: None,
                target_users: None,
                required_protocols: vec![community::Protocol::NativeDC],
                external_integrations: vec![],
                requires_ai_testimony: false,
                ai_agent_requirements: None,
                whitepaper_url: None,
                documentation_url: None,
                github_url: None,
                website_url: None,
                demo_url: None,
                team_members: vec![],
                milestones: vec![],
                funding_requested: 0,
                funding_currency: "FAT".to_string(),
                funding_breakdown: None,
                status: ProjectStatus::PendingReview,
                voting_starts_at: None,
                voting_ends_at: None,
                vote_count_for: 0,
                vote_count_against: 0,
                required_votes: 100,
                approval_threshold: 0.51,
                created_at: now,
                updated_at: now,
                approved_at: None,
                launched_at: None,
            }
        }
        
        /// Start voting period
        pub fn start_voting(&mut self, duration_seconds: u64) {
            let now = chrono::Utc::now().timestamp() as u64;
            self.status = ProjectStatus::Voting;
            self.voting_starts_at = Some(now);
            self.voting_ends_at = Some(now + duration_seconds);
            self.updated_at = now;
        }
        
        /// Add vote
        pub fn add_vote(&mut self, is_for: bool, weight: u64) {
            if is_for {
                self.vote_count_for += weight;
            } else {
                self.vote_count_against += weight;
            }
            self.updated_at = chrono::Utc::now().timestamp() as u64;
        }
        
        /// Check if voting has ended and determine result
        pub fn finalize_voting(&mut self) -> bool {
            let total_votes = self.vote_count_for + self.vote_count_against;
            
            if total_votes < self.required_votes {
                return false;
            }
            
            let approval_ratio = self.vote_count_for as f64 / total_votes as f64;
            
            if approval_ratio >= self.approval_threshold {
                self.status = ProjectStatus::Approved;
                self.approved_at = Some(chrono::Utc::now().timestamp() as u64);
                true
            } else {
                self.status = ProjectStatus::Rejected;
                false
            }
        }
        
        /// Mark project as building
        pub fn start_building(&mut self) {
            if self.status == ProjectStatus::Approved {
                self.status = ProjectStatus::Building;
                self.updated_at = chrono::Utc::now().timestamp() as u64;
            }
        }
        
        /// Mark project as launched
        pub fn launch(&mut self) {
            if self.status == ProjectStatus::Building {
                self.status = ProjectStatus::Launched;
                self.launched_at = Some(chrono::Utc::now().timestamp() as u64);
                self.updated_at = chrono::Utc::now().timestamp() as u64;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_community_creation() {
        let creator_id = [1u8; 32];
        let config = community::CommunityConfig::default();
        
        let community = community::Community::new(
            "Test Community".to_string(),
            "A test community".to_string(),
            creator_id,
            config,
        );
        
        assert_eq!(community.name, "Test Community");
        assert_eq!(community.status, community::CommunityStatus::PendingVote);
        assert_eq!(community.wallets_generated, 0);
    }
    
    #[test]
    fn test_wallet_generation() {
        let creator_id = [1u8; 32];
        let config = community::CommunityConfig {
            data_wallets_count: 100,
            ..Default::default()
        };
        
        let mut community = community::Community::new(
            "Test Community".to_string(),
            "A test community".to_string(),
            creator_id,
            config,
        );
        
        let wallets = community.generate_wallets(10);
        assert_eq!(wallets.len(), 10);
        assert_eq!(community.wallets_generated, 10);
        
        // Each wallet should have unique address
        let addresses: std::collections::HashSet<_> = wallets.iter().map(|w| w.address).collect();
        assert_eq!(addresses.len(), 10);
    }
    
    #[test]
    fn test_project_submission() {
        let submitter_id = [2u8; 32];
        
        let mut project = project::ProjectSubmission::new(
            "Test DeFi Project".to_string(),
            "A revolutionary DeFi protocol".to_string(),
            project::ProjectCategory::DeFi,
            submitter_id,
            project::OrganizationType::Business,
        );
        
        assert_eq!(project.status, project::ProjectStatus::PendingReview);
        
        // Start voting
        project.start_voting(7 * 24 * 60 * 60); // 7 days
        assert_eq!(project.status, project::ProjectStatus::Voting);
        
        // Add votes
        project.add_vote(true, 60);
        project.add_vote(false, 40);
        
        // Finalize
        let approved = project.finalize_voting();
        assert!(approved);
        assert_eq!(project.status, project::ProjectStatus::Approved);
    }
}
