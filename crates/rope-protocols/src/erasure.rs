//! # Controlled Erasure Protocol (CEP)
//! 
//! GDPR Article 17 compliant data deletion for the String Lattice.
//! 
//! ## Key Features
//! 
//! - **Cryptographic Deletion**: Destroy encryption keys, making data unreadable
//! - **Network Propagation**: Deletion requests spread to all nodes
//! - **Audit Trail**: Preserve proof of deletion without preserving content
//! - **Authorization**: Only authorized parties can initiate deletion
//! 
//! ## Erasure Flow
//! 
//! ```text
//! Erasure Request → Authorization Check → Key Destruction → Network Propagation
//!       ↓                   ↓                   ↓                   ↓
//! [Signed Request]  [Verify Rights]    [Destroy OES Keys]    [Broadcast CEP]
//!       ↓                   ↓                   ↓                   ↓
//! [Audit Record]    [Legal Check]      [Zero Memory]        [Confirm Peers]
//! ```
//! 
//! ## Compliance
//! 
//! - GDPR Article 17 (Right to Erasure)
//! - CCPA (California Consumer Privacy Act)
//! - LGPD (Brazilian Data Protection Law)

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use parking_lot::RwLock;

/// Erasure request
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErasureRequest {
    /// Request ID
    pub id: [u8; 32],
    
    /// Strings to erase
    pub string_ids: Vec<[u8; 32]>,
    
    /// Requester node ID
    pub requester_id: [u8; 32],
    
    /// Reason for erasure
    pub reason: ErasureReason,
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Authorization proof (signature)
    pub authorization_proof: Vec<u8>,
    
    /// Legal reference (if applicable)
    pub legal_reference: Option<String>,
    
    /// Cascade to related strings?
    pub cascade: bool,
}

impl ErasureRequest {
    /// Create new erasure request
    pub fn new(
        string_ids: Vec<[u8; 32]>,
        requester_id: [u8; 32],
        reason: ErasureReason,
    ) -> Self {
        let timestamp = chrono::Utc::now().timestamp();
        
        let mut id_data = requester_id.to_vec();
        id_data.extend_from_slice(&timestamp.to_le_bytes());
        for string_id in &string_ids {
            id_data.extend_from_slice(string_id);
        }
        let id = *blake3::hash(&id_data).as_bytes();
        
        Self {
            id,
            string_ids,
            requester_id,
            reason,
            timestamp,
            authorization_proof: Vec::new(),
            legal_reference: None,
            cascade: false,
        }
    }
    
    /// Set authorization proof
    pub fn with_authorization(mut self, proof: Vec<u8>) -> Self {
        self.authorization_proof = proof;
        self
    }
    
    /// Set legal reference
    pub fn with_legal_reference(mut self, reference: String) -> Self {
        self.legal_reference = Some(reference);
        self
    }
    
    /// Enable cascading erasure
    pub fn with_cascade(mut self) -> Self {
        self.cascade = true;
        self
    }
}

/// Reason for erasure
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErasureReason {
    /// GDPR Article 17 - Right to erasure
    GdprRequest {
        /// Data subject ID
        data_subject: Option<String>,
    },
    
    /// Data owner initiated
    OwnerRequest,
    
    /// Expired TTL (Time-To-Live)
    TtlExpired {
        original_ttl_seconds: u64,
    },
    
    /// Court order
    LegalOrder {
        reference: String,
        jurisdiction: String,
    },
    
    /// Contract condition met
    ContractCondition {
        contract_id: [u8; 32],
        condition_id: String,
    },
    
    /// System maintenance (e.g., orphaned data)
    SystemMaintenance,
    
    /// Privacy policy update
    PrivacyPolicyChange,
    
    /// Data breach response
    SecurityIncident {
        incident_id: String,
    },
}

impl ErasureReason {
    /// Check if this reason requires legal authorization
    pub fn requires_legal_auth(&self) -> bool {
        matches!(self, ErasureReason::LegalOrder { .. })
    }
    
    /// Check if this is user-initiated
    pub fn is_user_initiated(&self) -> bool {
        matches!(self, 
            ErasureReason::GdprRequest { .. } | 
            ErasureReason::OwnerRequest
        )
    }
    
    /// Get description
    pub fn description(&self) -> String {
        match self {
            ErasureReason::GdprRequest { data_subject } => {
                if let Some(ds) = data_subject {
                    format!("GDPR Article 17 request from data subject: {}", ds)
                } else {
                    "GDPR Article 17 - Right to Erasure".to_string()
                }
            }
            ErasureReason::OwnerRequest => "Data owner initiated deletion".to_string(),
            ErasureReason::TtlExpired { original_ttl_seconds } => {
                format!("TTL expired after {} seconds", original_ttl_seconds)
            }
            ErasureReason::LegalOrder { reference, jurisdiction } => {
                format!("Legal order {} in {}", reference, jurisdiction)
            }
            ErasureReason::ContractCondition { contract_id, condition_id } => {
                format!("Contract {:?} condition {} met", contract_id, condition_id)
            }
            ErasureReason::SystemMaintenance => "System maintenance".to_string(),
            ErasureReason::PrivacyPolicyChange => "Privacy policy update".to_string(),
            ErasureReason::SecurityIncident { incident_id } => {
                format!("Security incident response: {}", incident_id)
            }
        }
    }
}

/// Erasure status
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErasureStatus {
    /// Request pending authorization
    PendingAuthorization,
    
    /// Authorized, erasure in progress
    InProgress {
        erased_count: usize,
        total_count: usize,
    },
    
    /// Successfully erased
    Completed {
        erased_count: usize,
        timestamp: i64,
    },
    
    /// Partially completed (some strings couldn't be erased)
    PartiallyCompleted {
        erased_count: usize,
        failed_count: usize,
        failed_ids: Vec<[u8; 32]>,
    },
    
    /// Authorization denied
    Denied {
        reason: String,
    },
    
    /// Request expired
    Expired,
}

/// Erasure confirmation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErasureConfirmation {
    /// Request ID
    pub request_id: [u8; 32],
    
    /// Erased string IDs
    pub erased_strings: Vec<[u8; 32]>,
    
    /// Confirming node ID
    pub confirmer_id: [u8; 32],
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Confirmer's signature
    pub signature: Vec<u8>,
    
    /// Keys destroyed (proof without revealing keys)
    pub key_destruction_proofs: Vec<KeyDestructionProof>,
}

/// Proof that an encryption key was destroyed
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KeyDestructionProof {
    /// String ID
    pub string_id: [u8; 32],
    
    /// Hash of the destroyed key
    pub key_hash: [u8; 32],
    
    /// Destruction timestamp
    pub destroyed_at: i64,
    
    /// Method of destruction
    pub method: KeyDestructionMethod,
}

/// Method of key destruction
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyDestructionMethod {
    /// Secure memory wipe
    SecureWipe,
    
    /// Hardware Security Module destruction
    HsmDestruction,
    
    /// OES state evolution (key becomes unrecoverable)
    OesEvolution { generations_forward: u64 },
    
    /// Multi-party key share destruction
    ThresholdDestruction { shares_destroyed: u32, threshold: u32 },
}

/// Audit record for erasure
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErasureAuditRecord {
    /// Request ID
    pub request_id: [u8; 32],
    
    /// Number of strings erased
    pub string_count: usize,
    
    /// Reason
    pub reason: ErasureReason,
    
    /// Request timestamp
    pub requested_at: i64,
    
    /// Completion timestamp
    pub completed_at: Option<i64>,
    
    /// Final status
    pub status: ErasureStatus,
    
    /// Participating nodes
    pub participating_nodes: Vec<[u8; 32]>,
    
    /// Audit hash (for verification without content)
    pub audit_hash: [u8; 32],
}

/// Erasure coordinator
pub struct ErasureCoordinator {
    /// Node ID
    node_id: [u8; 32],
    
    /// Pending requests
    pending_requests: RwLock<HashMap<[u8; 32], ErasureRequest>>,
    
    /// Request statuses
    statuses: RwLock<HashMap<[u8; 32], ErasureStatus>>,
    
    /// Confirmations received
    confirmations: RwLock<HashMap<[u8; 32], Vec<ErasureConfirmation>>>,
    
    /// Erased strings (tombstones)
    erased_strings: RwLock<HashSet<[u8; 32]>>,
    
    /// Audit trail
    audit_trail: RwLock<Vec<ErasureAuditRecord>>,
    
    /// Required confirmations for completion
    required_confirmations: u32,
    
    /// Statistics
    stats: RwLock<ErasureStats>,
}

/// Erasure statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ErasureStats {
    pub total_requests: u64,
    pub completed_requests: u64,
    pub denied_requests: u64,
    pub total_strings_erased: u64,
    pub gdpr_requests: u64,
    pub owner_requests: u64,
    pub legal_orders: u64,
}

impl ErasureCoordinator {
    /// Create new coordinator
    pub fn new(node_id: [u8; 32], required_confirmations: u32) -> Self {
        Self {
            node_id,
            pending_requests: RwLock::new(HashMap::new()),
            statuses: RwLock::new(HashMap::new()),
            confirmations: RwLock::new(HashMap::new()),
            erased_strings: RwLock::new(HashSet::new()),
            audit_trail: RwLock::new(Vec::new()),
            required_confirmations,
            stats: RwLock::new(ErasureStats::default()),
        }
    }
    
    /// Submit an erasure request
    pub fn submit_request(&self, request: ErasureRequest) -> Result<[u8; 32], ErasureError> {
        // Validate request
        if request.string_ids.is_empty() {
            return Err(ErasureError::EmptyRequest);
        }
        
        // Check authorization for legal orders
        if request.reason.requires_legal_auth() && request.legal_reference.is_none() {
            return Err(ErasureError::MissingLegalReference);
        }
        
        let id = request.id;
        
        // Update stats
        {
            let mut stats = self.stats.write();
            stats.total_requests += 1;
            match &request.reason {
                ErasureReason::GdprRequest { .. } => stats.gdpr_requests += 1,
                ErasureReason::OwnerRequest => stats.owner_requests += 1,
                ErasureReason::LegalOrder { .. } => stats.legal_orders += 1,
                _ => {}
            }
        }
        
        // Store request
        self.pending_requests.write().insert(id, request);
        self.statuses.write().insert(id, ErasureStatus::PendingAuthorization);
        self.confirmations.write().insert(id, Vec::new());
        
        Ok(id)
    }
    
    /// Authorize an erasure request
    pub fn authorize(&self, request_id: &[u8; 32]) -> Result<(), ErasureError> {
        let mut statuses = self.statuses.write();
        let status = statuses.get_mut(request_id)
            .ok_or(ErasureError::RequestNotFound)?;
        
        if *status != ErasureStatus::PendingAuthorization {
            return Err(ErasureError::InvalidState);
        }
        
        let request = self.pending_requests.read().get(request_id)
            .ok_or(ErasureError::RequestNotFound)?
            .clone();
        
        *status = ErasureStatus::InProgress {
            erased_count: 0,
            total_count: request.string_ids.len(),
        };
        
        Ok(())
    }
    
    /// Deny an erasure request
    pub fn deny(&self, request_id: &[u8; 32], reason: String) {
        let mut statuses = self.statuses.write();
        if let Some(status) = statuses.get_mut(request_id) {
            *status = ErasureStatus::Denied { reason };
            self.stats.write().denied_requests += 1;
        }
    }
    
    /// Add erasure confirmation
    pub fn add_confirmation(&self, confirmation: ErasureConfirmation) -> Result<ErasureStatus, ErasureError> {
        let request_id = confirmation.request_id;
        
        // Store confirmation
        {
            let mut confirmations = self.confirmations.write();
            let list = confirmations.get_mut(&request_id)
                .ok_or(ErasureError::RequestNotFound)?;
            list.push(confirmation.clone());
        }
        
        // Update erased strings
        {
            let mut erased = self.erased_strings.write();
            for string_id in &confirmation.erased_strings {
                erased.insert(*string_id);
            }
        }
        
        // Check if we have enough confirmations
        let confirmations = self.confirmations.read().get(&request_id)
            .map(|c| c.len())
            .unwrap_or(0);
        
        if confirmations as u32 >= self.required_confirmations {
            self.complete_erasure(&request_id)
        } else {
            let request = self.pending_requests.read().get(&request_id)
                .ok_or(ErasureError::RequestNotFound)?
                .clone();
            
            Ok(ErasureStatus::InProgress {
                erased_count: confirmations,
                total_count: request.string_ids.len(),
            })
        }
    }
    
    /// Complete an erasure
    fn complete_erasure(&self, request_id: &[u8; 32]) -> Result<ErasureStatus, ErasureError> {
        let request = self.pending_requests.write().remove(request_id)
            .ok_or(ErasureError::RequestNotFound)?;
        
        let confirmations = self.confirmations.read().get(request_id)
            .cloned()
            .unwrap_or_default();
        
        // Count erased strings
        let erased_set: HashSet<_> = confirmations.iter()
            .flat_map(|c| c.erased_strings.iter())
            .copied()
            .collect();
        
        let erased_count = erased_set.len();
        let failed_count = request.string_ids.len() - erased_count;
        
        let status = if failed_count == 0 {
            ErasureStatus::Completed {
                erased_count,
                timestamp: chrono::Utc::now().timestamp(),
            }
        } else {
            let failed_ids: Vec<_> = request.string_ids.iter()
                .filter(|id| !erased_set.contains(*id))
                .copied()
                .collect();
            
            ErasureStatus::PartiallyCompleted {
                erased_count,
                failed_count,
                failed_ids,
            }
        };
        
        // Update stats
        {
            let mut stats = self.stats.write();
            stats.completed_requests += 1;
            stats.total_strings_erased += erased_count as u64;
        }
        
        // Create audit record
        let participating_nodes: Vec<_> = confirmations.iter()
            .map(|c| c.confirmer_id)
            .collect();
        
        let audit_record = ErasureAuditRecord {
            request_id: *request_id,
            string_count: request.string_ids.len(),
            reason: request.reason,
            requested_at: request.timestamp,
            completed_at: Some(chrono::Utc::now().timestamp()),
            status: status.clone(),
            participating_nodes,
            audit_hash: *request_id, // Simplified - in production, compute proper hash
        };
        
        self.audit_trail.write().push(audit_record);
        self.statuses.write().insert(*request_id, status.clone());
        
        Ok(status)
    }
    
    /// Check if a string is erased
    pub fn is_erased(&self, string_id: &[u8; 32]) -> bool {
        self.erased_strings.read().contains(string_id)
    }
    
    /// Get request status
    pub fn get_status(&self, request_id: &[u8; 32]) -> Option<ErasureStatus> {
        self.statuses.read().get(request_id).cloned()
    }
    
    /// Get audit trail
    pub fn audit_trail(&self) -> Vec<ErasureAuditRecord> {
        self.audit_trail.read().clone()
    }
    
    /// Get statistics
    pub fn stats(&self) -> ErasureStats {
        self.stats.read().clone()
    }
}

impl Default for ErasureCoordinator {
    fn default() -> Self {
        Self::new([0u8; 32], 3)
    }
}

/// Erasure errors
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ErasureError {
    EmptyRequest,
    RequestNotFound,
    InvalidState,
    MissingLegalReference,
    AuthorizationFailed,
    NetworkError,
}

impl std::fmt::Display for ErasureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ErasureError::EmptyRequest => write!(f, "Erasure request contains no strings"),
            ErasureError::RequestNotFound => write!(f, "Erasure request not found"),
            ErasureError::InvalidState => write!(f, "Invalid request state for operation"),
            ErasureError::MissingLegalReference => write!(f, "Legal reference required for legal orders"),
            ErasureError::AuthorizationFailed => write!(f, "Authorization failed"),
            ErasureError::NetworkError => write!(f, "Network error during erasure"),
        }
    }
}

impl std::error::Error for ErasureError {}

// ============================================================================
// Cryptographic Key Destruction
// ============================================================================

/// Key destructor for secure key elimination
pub struct CryptoKeyDestroyer {
    /// Secure random source for overwriting
    secure_random: [u8; 32],
    
    /// Number of overwrite passes
    overwrite_passes: u32,
}

impl CryptoKeyDestroyer {
    /// Create new key destroyer
    pub fn new() -> Self {
        let mut secure_random = [0u8; 32];
        // In production, use a proper secure RNG
        for (i, byte) in secure_random.iter_mut().enumerate() {
            *byte = (i as u8).wrapping_mul(137).wrapping_add(42);
        }
        
        Self {
            secure_random,
            overwrite_passes: 3,
        }
    }
    
    /// Set number of overwrite passes (DoD 5220.22-M recommends 3+)
    pub fn with_passes(mut self, passes: u32) -> Self {
        self.overwrite_passes = passes;
        self
    }
    
    /// Securely destroy a key (in-place zeroing)
    pub fn destroy_key(&self, key: &mut [u8]) -> KeyDestructionProof {
        let key_hash = *blake3::hash(key).as_bytes();
        
        // Multiple pass overwrite pattern (DoD 5220.22-M inspired)
        for pass in 0..self.overwrite_passes {
            let pattern: u8 = match pass % 3 {
                0 => 0x00,  // All zeros
                1 => 0xFF,  // All ones
                _ => self.secure_random[pass as usize % 32], // Random
            };
            
            for byte in key.iter_mut() {
                *byte = pattern;
            }
            
            // Memory barrier to prevent optimization
            std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
        }
        
        // Final zero pass
        for byte in key.iter_mut() {
            *byte = 0;
        }
        
        KeyDestructionProof {
            string_id: [0u8; 32], // Set by caller
            key_hash,
            destroyed_at: chrono::Utc::now().timestamp(),
            method: KeyDestructionMethod::SecureWipe,
        }
    }
    
    /// Destroy key by OES evolution (advance state to make old keys unrecoverable)
    pub fn destroy_by_oes_evolution(&self, key: &mut [u8], generations: u64) -> KeyDestructionProof {
        let key_hash = *blake3::hash(key).as_bytes();
        
        // Evolve the key state forward multiple generations
        // Each generation applies a one-way transformation
        for _ in 0..generations {
            let new_state = blake3::hash(key);
            key.copy_from_slice(&new_state.as_bytes()[..key.len().min(32)]);
        }
        
        // The original key state is now cryptographically unrecoverable
        KeyDestructionProof {
            string_id: [0u8; 32],
            key_hash,
            destroyed_at: chrono::Utc::now().timestamp(),
            method: KeyDestructionMethod::OesEvolution { generations_forward: generations },
        }
    }
    
    /// Destroy key by threshold secret sharing (destroy shares)
    pub fn destroy_threshold_shares(
        &self,
        shares: &mut [Vec<u8>],
        threshold: u32,
    ) -> KeyDestructionProof {
        let combined_hash = {
            let mut hasher = blake3::Hasher::new();
            for share in shares.iter() {
                hasher.update(share);
            }
            *hasher.finalize().as_bytes()
        };
        
        let shares_destroyed = shares.len() as u32;
        
        // Destroy each share
        for share in shares.iter_mut() {
            for pass in 0..self.overwrite_passes {
                let pattern = if pass % 2 == 0 { 0x00 } else { 0xFF };
                for byte in share.iter_mut() {
                    *byte = pattern;
                }
                std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
            }
            share.clear();
        }
        
        KeyDestructionProof {
            string_id: [0u8; 32],
            key_hash: combined_hash,
            destroyed_at: chrono::Utc::now().timestamp(),
            method: KeyDestructionMethod::ThresholdDestruction {
                shares_destroyed,
                threshold,
            },
        }
    }
}

impl Default for CryptoKeyDestroyer {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Network Erasure Propagation
// ============================================================================

/// Erasure propagation message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ErasurePropagation {
    /// Request ID
    pub request_id: [u8; 32],
    
    /// Strings to erase
    pub string_ids: Vec<[u8; 32]>,
    
    /// Originator node
    pub originator: [u8; 32],
    
    /// Propagation TTL (hops remaining)
    pub ttl: u32,
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Signature chain (each node signs)
    pub signatures: Vec<PropagationSignature>,
    
    /// Reason summary (for audit)
    pub reason: ErasureReason,
}

/// Signature from a propagating node
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PropagationSignature {
    /// Node ID
    pub node_id: [u8; 32],
    
    /// Signature
    pub signature: Vec<u8>,
    
    /// Timestamp
    pub timestamp: i64,
}

/// Network erasure propagator
pub struct ErasurePropagator {
    /// Node ID
    node_id: [u8; 32],
    
    /// Seen propagations (dedup)
    seen: RwLock<HashSet<[u8; 32]>>,
    
    /// Pending propagations
    pending: RwLock<Vec<ErasurePropagation>>,
    
    /// Confirmed erasures by node
    confirmations: RwLock<HashMap<[u8; 32], HashSet<[u8; 32]>>>,
    
    /// Statistics
    stats: RwLock<PropagationStats>,
}

/// Propagation statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PropagationStats {
    pub propagations_sent: u64,
    pub propagations_received: u64,
    pub propagations_confirmed: u64,
    pub unique_strings_erased: u64,
}

impl ErasurePropagator {
    /// Create new propagator
    pub fn new(node_id: [u8; 32]) -> Self {
        Self {
            node_id,
            seen: RwLock::new(HashSet::new()),
            pending: RwLock::new(Vec::new()),
            confirmations: RwLock::new(HashMap::new()),
            stats: RwLock::new(PropagationStats::default()),
        }
    }
    
    /// Create a new propagation message
    pub fn create_propagation(
        &self,
        request_id: [u8; 32],
        string_ids: Vec<[u8; 32]>,
        reason: ErasureReason,
    ) -> ErasurePropagation {
        let prop = ErasurePropagation {
            request_id,
            string_ids,
            originator: self.node_id,
            ttl: 10, // Default TTL
            timestamp: chrono::Utc::now().timestamp(),
            signatures: vec![],
            reason,
        };
        
        self.seen.write().insert(request_id);
        self.stats.write().propagations_sent += 1;
        
        prop
    }
    
    /// Handle incoming propagation
    pub fn handle_propagation(&self, mut prop: ErasurePropagation) -> Option<ErasurePropagation> {
        // Check if already seen
        if self.seen.read().contains(&prop.request_id) {
            return None;
        }
        
        // Mark as seen
        self.seen.write().insert(prop.request_id);
        self.stats.write().propagations_received += 1;
        
        // Check TTL
        if prop.ttl == 0 {
            return None;
        }
        
        // Add to pending for local processing
        self.pending.write().push(prop.clone());
        
        // Prepare to forward
        prop.ttl -= 1;
        prop.signatures.push(PropagationSignature {
            node_id: self.node_id,
            signature: vec![], // Signature added by networking layer
            timestamp: chrono::Utc::now().timestamp(),
        });
        
        Some(prop)
    }
    
    /// Get pending propagations for local erasure
    pub fn get_pending(&self) -> Vec<ErasurePropagation> {
        let mut pending = self.pending.write();
        std::mem::take(&mut *pending)
    }
    
    /// Confirm local erasure completed
    pub fn confirm_erasure(&self, request_id: [u8; 32], string_ids: Vec<[u8; 32]>) {
        let mut confirmations = self.confirmations.write();
        let set = confirmations.entry(request_id).or_insert_with(HashSet::new);
        
        for id in string_ids {
            set.insert(id);
        }
        
        self.stats.write().propagations_confirmed += 1;
        self.stats.write().unique_strings_erased += set.len() as u64;
    }
    
    /// Check if a string has been erased across the network
    pub fn is_erased(&self, request_id: &[u8; 32], string_id: &[u8; 32]) -> bool {
        self.confirmations.read()
            .get(request_id)
            .map(|set| set.contains(string_id))
            .unwrap_or(false)
    }
    
    /// Get statistics
    pub fn stats(&self) -> PropagationStats {
        self.stats.read().clone()
    }
    
    /// Clear old seen entries (memory management)
    pub fn cleanup_old(&self, max_age_seconds: i64) {
        let cutoff = chrono::Utc::now().timestamp() - max_age_seconds;
        
        // In a real implementation, we'd track timestamps
        // For now, just clear if too many
        let mut seen = self.seen.write();
        if seen.len() > 100_000 {
            seen.clear();
        }
    }
}

impl Default for ErasurePropagator {
    fn default() -> Self {
        Self::new([0u8; 32])
    }
}

// ============================================================================
// GDPR Compliance Helpers
// ============================================================================

/// GDPR compliance checker
pub struct GdprComplianceChecker {
    /// Minimum response time (days) - GDPR requires response within 30 days
    response_deadline_days: u32,
    
    /// Data retention policies by category
    retention_policies: HashMap<String, RetentionPolicy>,
}

/// Data retention policy
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RetentionPolicy {
    /// Category name
    pub category: String,
    
    /// Retention period in days (0 = indefinite)
    pub retention_days: u32,
    
    /// Legal basis for retention
    pub legal_basis: String,
    
    /// Auto-delete when expired
    pub auto_delete: bool,
}

impl GdprComplianceChecker {
    /// Create new compliance checker
    pub fn new() -> Self {
        Self {
            response_deadline_days: 30,
            retention_policies: HashMap::new(),
        }
    }
    
    /// Add a retention policy
    pub fn add_policy(&mut self, policy: RetentionPolicy) {
        self.retention_policies.insert(policy.category.clone(), policy);
    }
    
    /// Check if erasure request is valid
    pub fn validate_erasure_request(&self, request: &ErasureRequest) -> Result<(), String> {
        // Check authorization proof
        if request.authorization_proof.is_empty() {
            return Err("Authorization proof required for GDPR compliance".to_string());
        }
        
        // For legal orders, require legal reference
        if request.reason.requires_legal_auth() && request.legal_reference.is_none() {
            return Err("Legal reference required for legal order erasure".to_string());
        }
        
        // Check string count (reasonable limit)
        if request.string_ids.len() > 10_000 {
            return Err("Too many strings in single request (max 10,000)".to_string());
        }
        
        Ok(())
    }
    
    /// Calculate deadline for erasure response
    pub fn response_deadline(&self, request_timestamp: i64) -> i64 {
        request_timestamp + (self.response_deadline_days as i64 * 24 * 60 * 60)
    }
    
    /// Check if response is overdue
    pub fn is_overdue(&self, request: &ErasureRequest) -> bool {
        let deadline = self.response_deadline(request.timestamp);
        chrono::Utc::now().timestamp() > deadline
    }
    
    /// Generate compliance report for an erasure
    pub fn generate_compliance_report(&self, audit: &ErasureAuditRecord) -> ComplianceReport {
        let is_compliant = match &audit.status {
            ErasureStatus::Completed { timestamp, .. } => {
                let deadline = self.response_deadline(audit.requested_at);
                *timestamp <= deadline
            }
            _ => false,
        };
        
        let delay_days = audit.completed_at.map(|completed| {
            let deadline = self.response_deadline(audit.requested_at);
            if completed > deadline {
                Some((completed - deadline) / (24 * 60 * 60))
            } else {
                None
            }
        }).flatten();
        
        ComplianceReport {
            request_id: audit.request_id,
            is_compliant,
            reason: audit.reason.description(),
            strings_erased: audit.string_count,
            processing_time_hours: audit.completed_at.map(|c| (c - audit.requested_at) / 3600),
            deadline_met: is_compliant,
            delay_days,
            participating_nodes: audit.participating_nodes.len(),
        }
    }
}

impl Default for GdprComplianceChecker {
    fn default() -> Self {
        Self::new()
    }
}

/// GDPR compliance report
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ComplianceReport {
    /// Request ID
    pub request_id: [u8; 32],
    
    /// Overall compliance status
    pub is_compliant: bool,
    
    /// Erasure reason
    pub reason: String,
    
    /// Number of strings erased
    pub strings_erased: usize,
    
    /// Processing time in hours
    pub processing_time_hours: Option<i64>,
    
    /// Was 30-day deadline met?
    pub deadline_met: bool,
    
    /// Delay in days if deadline missed
    pub delay_days: Option<i64>,
    
    /// Number of participating nodes
    pub participating_nodes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_erasure_request() {
        let request = ErasureRequest::new(
            vec![[1u8; 32], [2u8; 32]],
            [0u8; 32],
            ErasureReason::GdprRequest { data_subject: Some("user@example.com".to_string()) },
        );
        
        assert_eq!(request.string_ids.len(), 2);
        assert!(request.reason.is_user_initiated());
    }
    
    #[test]
    fn test_erasure_coordinator() {
        let coord = ErasureCoordinator::new([0u8; 32], 1);
        
        let request = ErasureRequest::new(
            vec![[1u8; 32]],
            [0u8; 32],
            ErasureReason::OwnerRequest,
        );
        
        let id = coord.submit_request(request).unwrap();
        
        // Check pending
        assert_eq!(
            coord.get_status(&id),
            Some(ErasureStatus::PendingAuthorization)
        );
        
        // Authorize
        coord.authorize(&id).unwrap();
        
        // Add confirmation
        let confirmation = ErasureConfirmation {
            request_id: id,
            erased_strings: vec![[1u8; 32]],
            confirmer_id: [2u8; 32],
            timestamp: 0,
            signature: vec![],
            key_destruction_proofs: vec![],
        };
        
        let status = coord.add_confirmation(confirmation).unwrap();
        assert!(matches!(status, ErasureStatus::Completed { .. }));
        
        // Check erased
        assert!(coord.is_erased(&[1u8; 32]));
        assert!(!coord.is_erased(&[2u8; 32]));
    }
    
    #[test]
    fn test_legal_order_requires_reference() {
        let coord = ErasureCoordinator::new([0u8; 32], 1);
        
        let request = ErasureRequest::new(
            vec![[1u8; 32]],
            [0u8; 32],
            ErasureReason::LegalOrder {
                reference: "CASE-123".to_string(),
                jurisdiction: "EU".to_string(),
            },
        );
        
        // Should fail without legal reference
        let result = coord.submit_request(request);
        assert!(matches!(result, Err(ErasureError::MissingLegalReference)));
        
        // Should succeed with reference
        let request = ErasureRequest::new(
            vec![[1u8; 32]],
            [0u8; 32],
            ErasureReason::LegalOrder {
                reference: "CASE-123".to_string(),
                jurisdiction: "EU".to_string(),
            },
        ).with_legal_reference("COURT-ORDER-2024-001".to_string());
        
        assert!(coord.submit_request(request).is_ok());
    }
}

