//! # Advanced Protocols
//! 
//! DNA-inspired protocols for the String Lattice:
//! - **Regeneration**: Repair damaged/lost strings
//! - **Erasure**: Controlled deletion (GDPR compliant)
//! - **Gossip**: Gossip-about-gossip communication

pub mod regeneration {
    //! Regeneration Protocol - DNA-inspired repair
    //! 
    //! Repair strategies:
    //! - SingleNucleotide (BER analog)
    //! - SegmentCorruption (NER analog)
    //! - MismatchError (MMR analog)
    //! - SevereCorruption (DSB repair analog)
    
    use serde::{Deserialize, Serialize};
    
    /// Damage type detected in a string
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum DamageType {
        /// Single nucleotide error (1-32 bytes)
        SingleNucleotide { offset: usize },
        /// Segment corruption (multiple nucleotides)
        SegmentCorruption { start: usize, end: usize },
        /// Hash mismatch error
        MismatchError,
        /// Severe corruption (>50% data lost)
        SevereCorruption,
        /// Complete loss (need full regeneration)
        TotalLoss,
    }
    
    /// Repair request
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct RepairRequest {
        pub string_id: [u8; 32],
        pub damage_type: DamageType,
        pub requester_id: [u8; 32],
        pub timestamp: u64,
    }
    
    /// Repair response from a peer
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct RepairResponse {
        pub string_id: [u8; 32],
        pub repair_data: Vec<u8>,
        pub provider_id: [u8; 32],
        pub proof: Vec<u8>,
    }
    
    /// Regeneration coordinator
    pub struct RegenerationCoordinator {
        pending_repairs: Vec<RepairRequest>,
        successful_repairs: u64,
        failed_repairs: u64,
    }
    
    impl RegenerationCoordinator {
        pub fn new() -> Self {
            Self {
                pending_repairs: Vec::new(),
                successful_repairs: 0,
                failed_repairs: 0,
            }
        }
        
        pub fn request_repair(&mut self, request: RepairRequest) {
            self.pending_repairs.push(request);
        }
        
        pub fn mark_success(&mut self, _string_id: &[u8; 32]) {
            self.successful_repairs += 1;
        }
        
        pub fn mark_failure(&mut self, _string_id: &[u8; 32]) {
            self.failed_repairs += 1;
        }
        
        pub fn success_rate(&self) -> f64 {
            let total = self.successful_repairs + self.failed_repairs;
            if total == 0 {
                1.0
            } else {
                self.successful_repairs as f64 / total as f64
            }
        }
    }
    
    impl Default for RegenerationCoordinator {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod erasure {
    //! Controlled Erasure Protocol (CEP)
    //! 
    //! Enables GDPR Article 17 compliance through:
    //! - Cryptographic deletion (key erasure)
    //! - Network-wide propagation of deletion requests
    //! - Audit trail preservation
    
    use serde::{Deserialize, Serialize};
    
    /// Erasure request
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct ErasureRequest {
        pub string_ids: Vec<[u8; 32]>,
        pub requester_id: [u8; 32],
        pub reason: ErasureReason,
        pub timestamp: u64,
        pub authorization_proof: Vec<u8>,
    }
    
    /// Reason for erasure
    #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
    pub enum ErasureReason {
        /// GDPR Article 17 - Right to erasure
        GdprRequest,
        /// Data owner initiated
        OwnerRequest,
        /// Expired TTL
        TtlExpired,
        /// Court order
        LegalOrder { reference: String },
        /// System maintenance
        SystemMaintenance,
    }
    
    /// Erasure confirmation
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct ErasureConfirmation {
        pub request_id: [u8; 32],
        pub erased_strings: Vec<[u8; 32]>,
        pub confirmer_id: [u8; 32],
        pub timestamp: u64,
        pub signature: Vec<u8>,
    }
    
    /// Erasure coordinator
    pub struct ErasureCoordinator {
        pending_requests: Vec<ErasureRequest>,
        completed_erasures: Vec<[u8; 32]>,
    }
    
    impl ErasureCoordinator {
        pub fn new() -> Self {
            Self {
                pending_requests: Vec::new(),
                completed_erasures: Vec::new(),
            }
        }
        
        pub fn submit_request(&mut self, request: ErasureRequest) -> [u8; 32] {
            let request_id = blake3::hash(&serde_json::to_vec(&request).unwrap_or_default());
            self.pending_requests.push(request);
            *request_id.as_bytes()
        }
        
        pub fn confirm_erasure(&mut self, string_id: [u8; 32]) {
            self.completed_erasures.push(string_id);
        }
        
        pub fn is_erased(&self, string_id: &[u8; 32]) -> bool {
            self.completed_erasures.contains(string_id)
        }
    }
    
    impl Default for ErasureCoordinator {
        fn default() -> Self {
            Self::new()
        }
    }
}

pub mod gossip {
    //! Gossip-about-gossip protocol
    //! 
    //! Nodes share communication history for virtual voting.
    //! Each gossip event references its parents, forming a DAG.
    
    use serde::{Deserialize, Serialize};
    use std::collections::{HashMap, HashSet};
    
    /// A gossip event
    #[derive(Clone, Debug, Serialize, Deserialize)]
    pub struct GossipEvent {
        pub id: [u8; 32],
        pub creator_id: [u8; 32],
        pub self_parent: Option<[u8; 32]>,
        pub other_parent: Option<[u8; 32]>,
        pub payload: Vec<u8>,
        pub timestamp: u64,
        pub round: u64,
    }
    
    /// Gossip DAG for a node
    pub struct GossipDag {
        events: HashMap<[u8; 32], GossipEvent>,
        heads: HashSet<[u8; 32]>,
        round: u64,
    }
    
    impl GossipDag {
        pub fn new() -> Self {
            Self {
                events: HashMap::new(),
                heads: HashSet::new(),
                round: 0,
            }
        }
        
        pub fn add_event(&mut self, event: GossipEvent) {
            // Remove parents from heads
            if let Some(p) = event.self_parent {
                self.heads.remove(&p);
            }
            if let Some(p) = event.other_parent {
                self.heads.remove(&p);
            }
            
            let id = event.id;
            self.heads.insert(id);
            
            if event.round > self.round {
                self.round = event.round;
            }
            
            self.events.insert(id, event);
        }
        
        pub fn get_event(&self, id: &[u8; 32]) -> Option<&GossipEvent> {
            self.events.get(id)
        }
        
        pub fn current_round(&self) -> u64 {
            self.round
        }
        
        pub fn head_events(&self) -> Vec<&GossipEvent> {
            self.heads.iter()
                .filter_map(|id| self.events.get(id))
                .collect()
        }
    }
    
    impl Default for GossipDag {
        fn default() -> Self {
            Self::new()
        }
    }
}

// Re-exports
pub use regeneration::{DamageType, RepairRequest, RepairResponse, RegenerationCoordinator};
pub use erasure::{ErasureRequest, ErasureReason, ErasureConfirmation, ErasureCoordinator};
pub use gossip::{GossipEvent, GossipDag};
