//! # AI Testimony Agents
//! 
//! AI-powered agents that act as intelligent validators in the Smartchain.
//! They go beyond simple cryptographic verification to understand context,
//! validate business logic, and ensure contract conditions are met.
//! 
//! ## Role in Testimony Protocol
//! 
//! Traditional consensus: "Is the signature valid?"
//! AI Testimony: "Is the signature valid AND does this make business sense?"
//! 
//! ## Agent Types
//! 
//! - **ValidationAgent**: Validates transaction semantics and business rules
//! - **ContractAgent**: Monitors and enforces smart contract conditions
//! - **AnomalyAgent**: Detects suspicious patterns and fraud
//! - **ComplianceAgent**: Ensures regulatory compliance (KYC/AML/GDPR)
//! - **OracleAgent**: Bridges external data for contract evaluation

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// AI Testimony Agent trait - All AI agents must implement this
#[async_trait]
pub trait TestimonyAgent: Send + Sync {
    /// Unique agent identifier
    fn agent_id(&self) -> &AgentId;
    
    /// Agent type/role
    fn agent_type(&self) -> AgentType;
    
    /// Agent capabilities
    fn capabilities(&self) -> &[AgentCapability];
    
    /// Validate a contract condition
    async fn validate_condition(
        &self, 
        condition: &ContractCondition,
        context: &ValidationContext,
    ) -> ValidationResult;
    
    /// Provide testimony for a transaction
    async fn provide_testimony(
        &self,
        transaction: &TransactionRequest,
        context: &ValidationContext,
    ) -> Testimony;
    
    /// Health check
    async fn is_healthy(&self) -> bool;
}

/// Unique agent identifier
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId {
    pub uuid: Uuid,
    pub node_id: [u8; 32],
    pub public_key: Vec<u8>,
}

impl AgentId {
    pub fn new(node_id: [u8; 32], public_key: Vec<u8>) -> Self {
        Self {
            uuid: Uuid::new_v4(),
            node_id,
            public_key,
        }
    }
    
    pub fn to_bytes(&self) -> [u8; 32] {
        *blake3::hash(self.uuid.as_bytes()).as_bytes()
    }
}

/// Types of AI agents
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentType {
    /// Validates transactions and business logic
    Validation,
    /// Monitors smart contract conditions
    Contract,
    /// Detects anomalies and fraud
    Anomaly,
    /// Ensures regulatory compliance
    Compliance,
    /// Bridges external data sources
    Oracle,
    /// Custom agent type
    Custom(String),
}

/// Agent capabilities
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentCapability {
    /// Can validate insurance claims
    InsuranceClaim,
    /// Can validate financial transactions
    FinancialTransaction,
    /// Can perform KYC checks
    KycValidation,
    /// Can perform AML checks
    AmlScreening,
    /// Can validate asset transfers
    AssetTransfer,
    /// Can validate identity
    IdentityVerification,
    /// Can access external oracles
    OracleAccess(String),
    /// Can execute on specific protocol
    ProtocolExecution(String),
    /// Custom capability
    Custom(String),
}

/// A digitized contract with conditions
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DigitizedContract {
    /// Contract identifier (StringId in lattice)
    pub contract_id: [u8; 32],
    
    /// Contract parties
    pub parties: Vec<ContractParty>,
    
    /// Contract conditions
    pub conditions: Vec<ContractCondition>,
    
    /// Actions to execute when conditions are met
    pub actions: Vec<ContractAction>,
    
    /// Contract metadata
    pub metadata: ContractMetadata,
    
    /// Current state
    pub state: ContractState,
}

/// Party to a contract (linked to Datawallet)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractParty {
    /// Party's node ID (Datawallet identifier)
    pub node_id: [u8; 32],
    
    /// Party's public key
    pub public_key: Vec<u8>,
    
    /// Role in contract
    pub role: PartyRole,
    
    /// Party's signature on contract
    pub signature: Vec<u8>,
}

/// Role in a contract
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PartyRole {
    /// Primary party (e.g., insured, buyer)
    Primary,
    /// Counter-party (e.g., insurer, seller)
    CounterParty,
    /// Witness/validator
    Witness,
    /// Guarantor
    Guarantor,
    /// Custom role
    Custom(String),
}

/// Contract condition that must be validated by AI agents
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractCondition {
    /// Condition identifier
    pub id: [u8; 32],
    
    /// Condition type
    pub condition_type: ConditionType,
    
    /// Condition parameters
    pub parameters: HashMap<String, ConditionValue>,
    
    /// Required agent types to validate
    pub required_agents: Vec<AgentType>,
    
    /// Threshold of agent approvals needed (e.g., 2/3)
    pub approval_threshold: f64,
    
    /// Current validation status
    pub status: ConditionStatus,
}

/// Types of contract conditions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionType {
    /// Time-based condition (after date, duration)
    Temporal { trigger_after: i64 },
    
    /// Value threshold (amount >= X)
    ValueThreshold { 
        field: String, 
        operator: ComparisonOp, 
        value: i64 
    },
    
    /// External event (oracle-verified)
    ExternalEvent { 
        oracle_id: String, 
        event_type: String 
    },
    
    /// Multi-signature requirement
    MultiSig { 
        required_signatures: u32, 
        signers: Vec<[u8; 32]> 
    },
    
    /// Insurance claim validation
    InsuranceClaim {
        policy_id: String,
        claim_type: String,
    },
    
    /// KYC/AML compliance check
    ComplianceCheck {
        check_type: ComplianceCheckType,
    },
    
    /// Custom condition with expression
    Custom { 
        expression: String, 
        evaluator: String 
    },
}

/// Comparison operators for conditions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComparisonOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

/// Compliance check types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComplianceCheckType {
    Kyc,
    Aml,
    Pep,
    Sanctions,
    All,
}

/// Condition parameter values
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConditionValue {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Bytes(Vec<u8>),
    Array(Vec<ConditionValue>),
    Object(HashMap<String, ConditionValue>),
}

/// Status of a condition
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConditionStatus {
    /// Not yet evaluated
    Pending,
    /// Being evaluated by agents
    Evaluating { agents_responded: u32, total_agents: u32 },
    /// Condition is satisfied
    Satisfied { timestamp: i64, proofs: Vec<[u8; 32]> },
    /// Condition is not satisfied
    NotSatisfied { reason: String },
    /// Evaluation failed
    Error { error: String },
}

/// Action to execute when conditions are met
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractAction {
    /// Action identifier
    pub id: [u8; 32],
    
    /// Action type
    pub action_type: ActionType,
    
    /// Target protocol for execution
    pub target_protocol: TargetProtocol,
    
    /// Action parameters
    pub parameters: HashMap<String, ConditionValue>,
    
    /// Execution status
    pub status: ActionStatus,
}

/// Types of contract actions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionType {
    /// Transfer assets
    AssetTransfer,
    /// Make a payment
    Payment,
    /// Issue/burn tokens
    TokenOperation,
    /// Update contract state
    StateUpdate,
    /// Trigger another contract
    ContractCall,
    /// External API call
    ExternalCall,
    /// Custom action
    Custom(String),
}

/// Target protocol for action execution
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum TargetProtocol {
    /// Ethereum/EVM chain
    Ethereum { chain_id: u64, contract: String },
    /// Banking protocol
    Banking { protocol: String, account: String },
    /// Asset management
    AssetManagement { system: String },
    /// Internal Smartchain
    Internal,
    /// Custom protocol
    Custom { name: String, endpoint: String },
}

/// Action execution status
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionStatus {
    Pending,
    Executing,
    Completed { tx_hash: [u8; 32] },
    Failed { error: String },
}

/// Contract metadata
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContractMetadata {
    pub name: String,
    pub description: String,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub version: String,
    pub tags: Vec<String>,
}

/// Contract state
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContractState {
    /// Contract is being drafted
    Draft,
    /// Waiting for all parties to sign
    PendingSignatures,
    /// Active and monitoring conditions
    Active,
    /// Conditions met, executing actions
    Executing,
    /// All actions completed
    Completed { timestamp: i64 },
    /// Contract cancelled
    Cancelled { reason: String },
    /// Contract expired
    Expired,
    /// Contract disputed
    Disputed { dispute_id: [u8; 32] },
}

/// Validation context for agents
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationContext {
    /// Current timestamp
    pub timestamp: i64,
    
    /// Requesting entity
    pub requester: [u8; 32],
    
    /// Historical data available
    pub historical_data: HashMap<String, Vec<u8>>,
    
    /// External oracle data
    pub oracle_data: HashMap<String, OracleData>,
    
    /// Risk score from previous evaluations
    pub risk_score: Option<f64>,
}

/// Oracle data from external source
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OracleData {
    pub source: String,
    pub data: Vec<u8>,
    pub timestamp: i64,
    pub signature: Vec<u8>,
}

/// Result of condition validation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationResult {
    /// Is the condition satisfied?
    pub satisfied: bool,
    
    /// Confidence level (0.0 to 1.0)
    pub confidence: f64,
    
    /// Detailed reason
    pub reason: String,
    
    /// Supporting evidence
    pub evidence: Vec<Evidence>,
    
    /// Agent's signature on result
    pub signature: Vec<u8>,
}

/// Evidence supporting a validation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Evidence {
    pub evidence_type: EvidenceType,
    pub data: Vec<u8>,
    pub hash: [u8; 32],
    pub source: String,
}

/// Types of evidence
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EvidenceType {
    Document,
    Signature,
    OracleAttestation,
    TransactionProof,
    IdentityProof,
    Custom(String),
}

/// Transaction request to be validated
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransactionRequest {
    pub id: [u8; 32],
    pub contract_id: Option<[u8; 32]>,
    pub from: [u8; 32],
    pub to: [u8; 32],
    pub action: ActionType,
    pub parameters: HashMap<String, ConditionValue>,
    pub timestamp: i64,
}

/// AI Testimony - The agent's attestation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Testimony {
    /// Agent providing testimony
    pub agent_id: AgentId,
    
    /// Transaction/condition being testified
    pub subject_id: [u8; 32],
    
    /// Testimony type
    pub testimony_type: TestimonyType,
    
    /// Decision
    pub decision: TestimonyDecision,
    
    /// Confidence (0.0 to 1.0)
    pub confidence: f64,
    
    /// Reasoning (can be audited)
    pub reasoning: String,
    
    /// Timestamp
    pub timestamp: i64,
    
    /// Agent's signature
    pub signature: Vec<u8>,
}

/// Type of testimony
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestimonyType {
    /// Validating a transaction
    TransactionValidation,
    /// Validating a contract condition
    ConditionValidation,
    /// Compliance attestation
    ComplianceAttestation,
    /// Anomaly report
    AnomalyReport,
    /// Oracle data attestation
    OracleAttestation,
}

/// Testimony decision
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TestimonyDecision {
    Approve,
    Reject,
    NeedsMoreInfo,
    Abstain,
}

// === Concrete Agent Implementations ===

/// Validation Agent - Validates business logic
pub struct ValidationAgent {
    id: AgentId,
    capabilities: Vec<AgentCapability>,
}

impl ValidationAgent {
    pub fn new(node_id: [u8; 32], public_key: Vec<u8>) -> Self {
        Self {
            id: AgentId::new(node_id, public_key),
            capabilities: vec![
                AgentCapability::FinancialTransaction,
                AgentCapability::AssetTransfer,
            ],
        }
    }
}

#[async_trait]
impl TestimonyAgent for ValidationAgent {
    fn agent_id(&self) -> &AgentId {
        &self.id
    }
    
    fn agent_type(&self) -> AgentType {
        AgentType::Validation
    }
    
    fn capabilities(&self) -> &[AgentCapability] {
        &self.capabilities
    }
    
    async fn validate_condition(
        &self,
        condition: &ContractCondition,
        context: &ValidationContext,
    ) -> ValidationResult {
        // Basic validation logic - in production, this would use ML models
        let satisfied = match &condition.condition_type {
            ConditionType::Temporal { trigger_after } => {
                context.timestamp >= *trigger_after
            }
            ConditionType::ValueThreshold { field, operator, value } => {
                // Get value from parameters
                if let Some(ConditionValue::Integer(v)) = condition.parameters.get(field) {
                    match operator {
                        ComparisonOp::Eq => v == value,
                        ComparisonOp::Ne => v != value,
                        ComparisonOp::Gt => v > value,
                        ComparisonOp::Ge => v >= value,
                        ComparisonOp::Lt => v < value,
                        ComparisonOp::Le => v <= value,
                    }
                } else {
                    false
                }
            }
            _ => true, // Other conditions need specialized agents
        };
        
        ValidationResult {
            satisfied,
            confidence: if satisfied { 0.95 } else { 0.90 },
            reason: if satisfied { 
                "Condition validated".to_string() 
            } else { 
                "Condition not met".to_string() 
            },
            evidence: Vec::new(),
            signature: Vec::new(),
        }
    }
    
    async fn provide_testimony(
        &self,
        transaction: &TransactionRequest,
        _context: &ValidationContext,
    ) -> Testimony {
        Testimony {
            agent_id: self.id.clone(),
            subject_id: transaction.id,
            testimony_type: TestimonyType::TransactionValidation,
            decision: TestimonyDecision::Approve,
            confidence: 0.95,
            reasoning: "Transaction validated by ValidationAgent".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            signature: Vec::new(),
        }
    }
    
    async fn is_healthy(&self) -> bool {
        true
    }
}

/// Insurance Agent - Specialized for insurance claims
pub struct InsuranceAgent {
    id: AgentId,
    capabilities: Vec<AgentCapability>,
}

impl InsuranceAgent {
    pub fn new(node_id: [u8; 32], public_key: Vec<u8>) -> Self {
        Self {
            id: AgentId::new(node_id, public_key),
            capabilities: vec![
                AgentCapability::InsuranceClaim,
                AgentCapability::IdentityVerification,
            ],
        }
    }
}

#[async_trait]
impl TestimonyAgent for InsuranceAgent {
    fn agent_id(&self) -> &AgentId {
        &self.id
    }
    
    fn agent_type(&self) -> AgentType {
        AgentType::Contract
    }
    
    fn capabilities(&self) -> &[AgentCapability] {
        &self.capabilities
    }
    
    async fn validate_condition(
        &self,
        condition: &ContractCondition,
        context: &ValidationContext,
    ) -> ValidationResult {
        match &condition.condition_type {
            ConditionType::InsuranceClaim { policy_id, claim_type } => {
                // In production: validate against policy, check evidence, use ML for fraud detection
                let has_evidence = !context.historical_data.is_empty();
                
                ValidationResult {
                    satisfied: has_evidence,
                    confidence: if has_evidence { 0.85 } else { 0.30 },
                    reason: format!(
                        "Insurance claim for policy {} (type: {}) - Evidence: {}",
                        policy_id, claim_type, if has_evidence { "present" } else { "missing" }
                    ),
                    evidence: Vec::new(),
                    signature: Vec::new(),
                }
            }
            _ => ValidationResult {
                satisfied: false,
                confidence: 0.0,
                reason: "Not an insurance condition".to_string(),
                evidence: Vec::new(),
                signature: Vec::new(),
            }
        }
    }
    
    async fn provide_testimony(
        &self,
        transaction: &TransactionRequest,
        _context: &ValidationContext,
    ) -> Testimony {
        Testimony {
            agent_id: self.id.clone(),
            subject_id: transaction.id,
            testimony_type: TestimonyType::ConditionValidation,
            decision: TestimonyDecision::Approve,
            confidence: 0.85,
            reasoning: "Insurance claim validated by InsuranceAgent".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            signature: Vec::new(),
        }
    }
    
    async fn is_healthy(&self) -> bool {
        true
    }
}

/// Compliance Agent - KYC/AML validation
pub struct ComplianceAgent {
    id: AgentId,
    capabilities: Vec<AgentCapability>,
}

impl ComplianceAgent {
    pub fn new(node_id: [u8; 32], public_key: Vec<u8>) -> Self {
        Self {
            id: AgentId::new(node_id, public_key),
            capabilities: vec![
                AgentCapability::KycValidation,
                AgentCapability::AmlScreening,
                AgentCapability::IdentityVerification,
            ],
        }
    }
}

#[async_trait]
impl TestimonyAgent for ComplianceAgent {
    fn agent_id(&self) -> &AgentId {
        &self.id
    }
    
    fn agent_type(&self) -> AgentType {
        AgentType::Compliance
    }
    
    fn capabilities(&self) -> &[AgentCapability] {
        &self.capabilities
    }
    
    async fn validate_condition(
        &self,
        condition: &ContractCondition,
        _context: &ValidationContext,
    ) -> ValidationResult {
        match &condition.condition_type {
            ConditionType::ComplianceCheck { check_type } => {
                // In production: integrate with KYC/AML providers
                ValidationResult {
                    satisfied: true,
                    confidence: 0.99,
                    reason: format!("{:?} check passed", check_type),
                    evidence: Vec::new(),
                    signature: Vec::new(),
                }
            }
            _ => ValidationResult {
                satisfied: true,
                confidence: 1.0,
                reason: "No compliance requirements".to_string(),
                evidence: Vec::new(),
                signature: Vec::new(),
            }
        }
    }
    
    async fn provide_testimony(
        &self,
        transaction: &TransactionRequest,
        _context: &ValidationContext,
    ) -> Testimony {
        Testimony {
            agent_id: self.id.clone(),
            subject_id: transaction.id,
            testimony_type: TestimonyType::ComplianceAttestation,
            decision: TestimonyDecision::Approve,
            confidence: 0.99,
            reasoning: "Compliance checks passed".to_string(),
            timestamp: chrono::Utc::now().timestamp(),
            signature: Vec::new(),
        }
    }
    
    async fn is_healthy(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_validation_agent() {
        let agent = ValidationAgent::new([0u8; 32], vec![]);
        
        assert_eq!(agent.agent_type(), AgentType::Validation);
        assert!(agent.is_healthy().await);
    }
    
    #[tokio::test]
    async fn test_temporal_condition() {
        let agent = ValidationAgent::new([0u8; 32], vec![]);
        
        let condition = ContractCondition {
            id: [0u8; 32],
            condition_type: ConditionType::Temporal { 
                trigger_after: 1000 
            },
            parameters: HashMap::new(),
            required_agents: vec![AgentType::Validation],
            approval_threshold: 0.5,
            status: ConditionStatus::Pending,
        };
        
        let context = ValidationContext {
            timestamp: 2000, // After trigger
            requester: [0u8; 32],
            historical_data: HashMap::new(),
            oracle_data: HashMap::new(),
            risk_score: None,
        };
        
        let result = agent.validate_condition(&condition, &context).await;
        assert!(result.satisfied);
    }
}

