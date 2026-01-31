//! Enhanced AI Testimony
//!
//! AI-powered semantic validation for the Testimony Protocol.
//! Extends basic cryptographic testimonies with semantic understanding,
//! risk assessment, and multi-agent consensus.

use crate::testimony::{Testimony, TestimonySignature};
use rope_core::types::{NodeId, StringId};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// AI-Enhanced Testimony for semantic validation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AITestimony {
    /// Base cryptographic testimony
    pub base: Testimony,

    /// AI Agent identity
    pub agent_id: AgentId,

    /// Agent type providing testimony
    pub agent_type: AIAgentType,

    /// Semantic verdict
    pub verdict: SemanticVerdict,

    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,

    /// Encrypted reasoning trace (for audit)
    pub reasoning_ciphertext: Vec<u8>,

    /// Evidence references (StringIds of supporting data)
    pub evidence_refs: Vec<StringId>,

    /// Conditional approval terms
    pub conditions: Option<ApprovalConditions>,

    /// Risk assessment
    pub risk_assessment: RiskAssessment,
}

impl AITestimony {
    /// Create new AI testimony
    pub fn new(
        base: Testimony,
        agent_id: AgentId,
        agent_type: AIAgentType,
        verdict: SemanticVerdict,
        confidence: f64,
    ) -> Self {
        Self {
            base,
            agent_id,
            agent_type,
            verdict,
            confidence,
            reasoning_ciphertext: Vec::new(),
            evidence_refs: Vec::new(),
            conditions: None,
            risk_assessment: RiskAssessment::default(),
        }
    }

    /// Check if testimony approves the action
    pub fn is_approval(&self) -> bool {
        matches!(
            self.verdict,
            SemanticVerdict::Approve | SemanticVerdict::ConditionalApprove { .. }
        )
    }

    /// Get verdict as string
    pub fn verdict_string(&self) -> &'static str {
        match self.verdict {
            SemanticVerdict::Approve => "approve",
            SemanticVerdict::Reject { .. } => "reject",
            SemanticVerdict::Abstain => "abstain",
            SemanticVerdict::NeedsMoreInfo { .. } => "needs_more_info",
            SemanticVerdict::ConditionalApprove { .. } => "conditional_approve",
        }
    }

    /// Serialize for String Lattice storage
    pub fn serialize_content(&self) -> Vec<u8> {
        let mut content = Vec::new();

        // Type marker for AI testimony
        content.push(0x02); // AI_TESTIMONY_TYPE

        // Version
        content.push(0x01);

        // Base testimony ID
        content.extend_from_slice(&self.base.id);

        // Agent ID
        content.extend_from_slice(&self.agent_id.to_bytes());

        // Agent type (serialize as u8)
        content.push(self.agent_type.as_u8());

        // Verdict
        content.push(self.verdict.as_u8());

        // Confidence (as u16, scaled to 0-10000)
        let confidence_scaled = (self.confidence * 10000.0) as u16;
        content.extend_from_slice(&confidence_scaled.to_le_bytes());

        // Risk level
        content.push(self.risk_assessment.level.as_u8());

        // Risk score
        content.push(self.risk_assessment.score);

        // Reasoning ciphertext length and data
        let reasoning_len = self.reasoning_ciphertext.len() as u32;
        content.extend_from_slice(&reasoning_len.to_le_bytes());
        content.extend_from_slice(&self.reasoning_ciphertext);

        // Evidence count and refs
        content.push(self.evidence_refs.len() as u8);
        for evidence in &self.evidence_refs {
            content.extend_from_slice(evidence.as_bytes());
        }

        content
    }

    /// Compute String ID for this testimony
    pub fn compute_string_id(&self) -> StringId {
        let content = self.serialize_content();
        StringId::from_content(&content)
    }
}

/// AI Agent identifier
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentId {
    /// UUID
    pub uuid: [u8; 16],

    /// Node ID
    pub node_id: [u8; 32],

    /// Public key hash
    pub public_key_hash: [u8; 32],
}

impl AgentId {
    /// Create new agent ID
    pub fn new(node_id: [u8; 32], public_key: &[u8]) -> Self {
        let uuid = uuid::Uuid::new_v4();
        let public_key_hash = *blake3::hash(public_key).as_bytes();

        Self {
            uuid: *uuid.as_bytes(),
            node_id,
            public_key_hash,
        }
    }

    /// Convert to bytes
    pub fn to_bytes(&self) -> [u8; 32] {
        let mut data = Vec::new();
        data.extend_from_slice(&self.uuid);
        data.extend_from_slice(&self.node_id[..16]);

        *blake3::hash(&data).as_bytes()
    }

    /// Create from owner identity
    pub fn from_owner(node_id: [u8; 32]) -> Self {
        Self::new(node_id, &node_id)
    }
}

/// AI Agent types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AIAgentType {
    /// Validates transaction semantics and business logic
    Validation,

    /// Monitors smart contract conditions
    Contract,

    /// Detects anomalies and fraud
    Anomaly,

    /// Ensures regulatory compliance (KYC/AML/GDPR)
    Compliance,

    /// Bridges external data for contract evaluation
    Oracle { data_sources: Vec<String> },

    /// Executes approved actions via Tool Registry
    Execution { supported_protocols: Vec<String> },

    /// Post-execution verification and audit
    Audit { audit_scope: AuditScope },

    /// Personal AI assistant (OpenClaw-style)
    Personal { owner_id: [u8; 32] },

    /// Insurance claim evaluation
    Insurance { policy_types: Vec<String> },

    /// Custom agent type
    Custom(String),
}

impl AIAgentType {
    /// Convert to u8 for serialization
    pub fn as_u8(&self) -> u8 {
        match self {
            AIAgentType::Validation => 1,
            AIAgentType::Contract => 2,
            AIAgentType::Anomaly => 3,
            AIAgentType::Compliance => 4,
            AIAgentType::Oracle { .. } => 5,
            AIAgentType::Execution { .. } => 6,
            AIAgentType::Audit { .. } => 7,
            AIAgentType::Personal { .. } => 8,
            AIAgentType::Insurance { .. } => 9,
            AIAgentType::Custom(_) => 255,
        }
    }
}

/// Audit scope for audit agents
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuditScope {
    Financial,
    Compliance,
    Security,
    Operational,
    Full,
}

/// Semantic verdict from AI analysis
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SemanticVerdict {
    /// Action is approved
    Approve,

    /// Action is rejected with reason
    Reject { reason: String },

    /// Agent abstains (not qualified to judge)
    Abstain,

    /// Needs additional information
    NeedsMoreInfo { required: Vec<String> },

    /// Conditionally approved
    ConditionalApprove { conditions: Vec<String> },
}

impl SemanticVerdict {
    /// Convert to u8 for serialization
    pub fn as_u8(&self) -> u8 {
        match self {
            SemanticVerdict::Approve => 1,
            SemanticVerdict::Reject { .. } => 2,
            SemanticVerdict::Abstain => 3,
            SemanticVerdict::NeedsMoreInfo { .. } => 4,
            SemanticVerdict::ConditionalApprove { .. } => 5,
        }
    }
}

/// Approval conditions for conditional verdicts
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApprovalConditions {
    /// Time-bound conditions
    pub temporal: Option<TemporalCondition>,

    /// Value limits
    pub value_limits: Option<ValueLimit>,

    /// Required additional approvals
    pub required_approvers: Vec<AgentId>,

    /// Custom conditions
    pub custom: Vec<String>,
}

/// Temporal condition
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemporalCondition {
    /// Valid from timestamp
    pub valid_from: i64,

    /// Valid until timestamp
    pub valid_until: i64,
}

/// Value limit
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValueLimit {
    /// Maximum value
    pub max_value: u64,

    /// Currency
    pub currency: String,
}

/// Risk assessment from AI agent
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskAssessment {
    /// Overall risk level
    pub level: RiskLevel,

    /// Risk score (0-100)
    pub score: u8,

    /// Identified risk factors
    pub factors: Vec<RiskFactor>,

    /// Recommended mitigations
    pub mitigations: Vec<String>,
}

impl Default for RiskAssessment {
    fn default() -> Self {
        Self {
            level: RiskLevel::Low,
            score: 0,
            factors: Vec::new(),
            mitigations: Vec::new(),
        }
    }
}

/// Risk levels
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskLevel {
    #[default]
    Low,
    Medium,
    High,
    Critical,
}

impl RiskLevel {
    /// Convert to u8
    pub fn as_u8(&self) -> u8 {
        match self {
            RiskLevel::Low => 1,
            RiskLevel::Medium => 2,
            RiskLevel::High => 3,
            RiskLevel::Critical => 4,
        }
    }

    /// Create from score
    pub fn from_score(score: u8) -> Self {
        match score {
            0..=25 => RiskLevel::Low,
            26..=50 => RiskLevel::Medium,
            51..=75 => RiskLevel::High,
            _ => RiskLevel::Critical,
        }
    }
}

/// Risk factor
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskFactor {
    /// Factor type
    pub factor_type: RiskFactorType,

    /// Severity (1-10)
    pub severity: u8,

    /// Description
    pub description: String,
}

/// Risk factor types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskFactorType {
    /// High transaction value
    HighValue,

    /// Unusual behavior pattern
    AnomalousPattern,

    /// Compliance concern
    ComplianceRisk,

    /// Counterparty risk
    CounterpartyRisk,

    /// Smart contract vulnerability
    ContractRisk,

    /// External data reliability
    OracleRisk,

    /// Custom risk
    Custom(String),
}

/// AI Testimony collector (extends TestimonyCollector)
pub struct AITestimonyCollector {
    /// Collections by action ID
    collections: parking_lot::RwLock<HashMap<StringId, AITestimonyCollection>>,

    /// Known agents
    agents: parking_lot::RwLock<Vec<AgentId>>,

    /// Configuration
    config: AITestimonyConfig,
}

/// AI Testimony collection for an action
#[derive(Clone, Debug, Default)]
pub struct AITestimonyCollection {
    /// Action string ID
    pub action_id: StringId,

    /// Collected testimonies
    pub testimonies: Vec<AITestimony>,

    /// Approval count
    pub approvals: u32,

    /// Rejection count
    pub rejections: u32,

    /// Abstain count
    pub abstains: u32,

    /// Average confidence
    pub avg_confidence: f64,

    /// Highest risk level seen
    pub max_risk_level: RiskLevel,

    /// Is consensus reached
    pub consensus_reached: bool,

    /// Consensus result
    pub consensus_result: Option<ConsensusResult>,
}

impl AITestimonyCollection {
    /// Create new collection
    pub fn new(action_id: StringId) -> Self {
        Self {
            action_id,
            ..Default::default()
        }
    }

    /// Add testimony
    pub fn add(&mut self, testimony: AITestimony) {
        // Update counts
        match &testimony.verdict {
            SemanticVerdict::Approve | SemanticVerdict::ConditionalApprove { .. } => {
                self.approvals += 1;
            }
            SemanticVerdict::Reject { .. } => {
                self.rejections += 1;
            }
            SemanticVerdict::Abstain | SemanticVerdict::NeedsMoreInfo { .. } => {
                self.abstains += 1;
            }
        }

        // Update risk level
        if testimony.risk_assessment.level.as_u8() > self.max_risk_level.as_u8() {
            self.max_risk_level = testimony.risk_assessment.level.clone();
        }

        // Update average confidence
        let n = self.testimonies.len() as f64;
        self.avg_confidence = (self.avg_confidence * n + testimony.confidence) / (n + 1.0);

        self.testimonies.push(testimony);
    }

    /// Check if consensus threshold is reached
    pub fn check_consensus(&mut self, min_approvals: u32, min_confidence: f64, max_risk: RiskLevel) -> bool {
        if self.approvals >= min_approvals
            && self.avg_confidence >= min_confidence
            && self.max_risk_level.as_u8() <= max_risk.as_u8()
        {
            self.consensus_reached = true;
            self.consensus_result = Some(ConsensusResult::Approved);
        } else if self.rejections >= min_approvals {
            self.consensus_reached = true;
            self.consensus_result = Some(ConsensusResult::Rejected {
                reasons: self
                    .testimonies
                    .iter()
                    .filter_map(|t| {
                        if let SemanticVerdict::Reject { reason } = &t.verdict {
                            Some(reason.clone())
                        } else {
                            None
                        }
                    })
                    .collect(),
            });
        }

        self.consensus_reached
    }
}

/// Consensus result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ConsensusResult {
    /// Action approved
    Approved,

    /// Action rejected
    Rejected { reasons: Vec<String> },

    /// Needs more information
    NeedsMoreInfo { required: Vec<String> },

    /// Timed out
    TimedOut,
}

/// AI Testimony configuration
#[derive(Clone, Debug)]
pub struct AITestimonyConfig {
    /// Minimum approvals for consensus
    pub min_approvals: u32,

    /// Minimum confidence score
    pub min_confidence: f64,

    /// Maximum acceptable risk level
    pub max_risk_level: RiskLevel,

    /// Timeout for testimony collection (seconds)
    pub timeout_secs: u64,
}

impl Default for AITestimonyConfig {
    fn default() -> Self {
        Self {
            min_approvals: 3,
            min_confidence: 0.8,
            max_risk_level: RiskLevel::Medium,
            timeout_secs: 30,
        }
    }
}

impl AITestimonyCollector {
    /// Create new collector
    pub fn new(config: AITestimonyConfig) -> Self {
        Self {
            collections: parking_lot::RwLock::new(HashMap::new()),
            agents: parking_lot::RwLock::new(Vec::new()),
            config,
        }
    }

    /// Register an AI agent
    pub fn register_agent(&self, agent_id: AgentId) {
        let mut agents = self.agents.write();
        if !agents.iter().any(|a| a == &agent_id) {
            agents.push(agent_id);
        }
    }

    /// Submit AI testimony
    pub fn submit_testimony(&self, testimony: AITestimony) -> bool {
        let action_id = testimony.base.target_string_id;

        let mut collections = self.collections.write();
        let collection = collections
            .entry(action_id)
            .or_insert_with(|| AITestimonyCollection::new(action_id));

        collection.add(testimony);

        // Check consensus
        collection.check_consensus(
            self.config.min_approvals,
            self.config.min_confidence,
            self.config.max_risk_level.clone(),
        )
    }

    /// Get collection for action
    pub fn get_collection(&self, action_id: &StringId) -> Option<AITestimonyCollection> {
        self.collections.read().get(action_id).cloned()
    }

    /// Check if action has consensus
    pub fn has_consensus(&self, action_id: &StringId) -> bool {
        self.collections
            .read()
            .get(action_id)
            .map(|c| c.consensus_reached)
            .unwrap_or(false)
    }

    /// Get consensus result
    pub fn consensus_result(&self, action_id: &StringId) -> Option<ConsensusResult> {
        self.collections
            .read()
            .get(action_id)
            .and_then(|c| c.consensus_result.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testimony::Testimony;
    use rope_core::clock::LamportClock;
    use rope_core::types::AttestationType;

    fn test_testimony() -> Testimony {
        let string_id = StringId::from_content(b"test");
        let validator_id = NodeId::new([1u8; 32]);
        let timestamp = LamportClock::new(validator_id);

        Testimony::new(
            string_id,
            validator_id,
            AttestationType::Existence,
            timestamp,
            1,
        )
    }

    #[test]
    fn test_ai_testimony_creation() {
        let base = test_testimony();
        let agent_id = AgentId::new([1u8; 32], &[0u8; 64]);

        let testimony = AITestimony::new(
            base,
            agent_id,
            AIAgentType::Validation,
            SemanticVerdict::Approve,
            0.95,
        );

        assert!(testimony.is_approval());
        assert_eq!(testimony.confidence, 0.95);
    }

    #[test]
    fn test_ai_testimony_collection() {
        let config = AITestimonyConfig {
            min_approvals: 2,
            min_confidence: 0.8,
            max_risk_level: RiskLevel::Medium,
            timeout_secs: 30,
        };

        let collector = AITestimonyCollector::new(config);

        // Submit 2 approving testimonies
        for i in 0..2 {
            let base = test_testimony();
            let agent_id = AgentId::new([i as u8; 32], &[0u8; 64]);

            let testimony = AITestimony::new(
                base,
                agent_id,
                AIAgentType::Validation,
                SemanticVerdict::Approve,
                0.9,
            );

            let result = collector.submit_testimony(testimony);

            if i == 1 {
                assert!(result); // Consensus reached on second testimony
            }
        }

        let action_id = StringId::from_content(b"test");
        assert!(collector.has_consensus(&action_id));
        assert!(matches!(
            collector.consensus_result(&action_id),
            Some(ConsensusResult::Approved)
        ));
    }
}
