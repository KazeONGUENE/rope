//! AI Agent Runner
//!
//! Instantiates and manages the built-in AI testimony agents that participate
//! in the consensus pipeline. When a transaction is notarized, the agent runner
//! dispatches it to all active agents for validation and testimony.
//!
//! Agents:
//! - ValidationAgent: checks transaction semantics and business rules
//! - ComplianceAgent: KYC/AML regulatory checks
//! - InsuranceAgent: insurance claim validation

use rope_consensus::{
    AIAgentType, AITestimony, AITestimonyCollector, AITestimonyConfig, AgentId as ConsensusAgentId,
    RiskLevel, SemanticVerdict, Testimony as ConsensusTestimony,
};
use rope_core::clock::LamportClock;
use rope_core::types::{AttestationType, NodeId, StringId};
use rope_smartchain::testimony_agent::{
    ActionType, AgentType, ComplianceAgent, InsuranceAgent, TestimonyAgent, TestimonyDecision,
    TransactionRequest, ValidationAgent, ValidationContext,
};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;
use tracing::{debug, warn};

/// Events from the agent runner
#[derive(Clone, Debug)]
pub enum AgentEvent {
    TestimonyProvided {
        agent_type: String,
        string_id: StringId,
        approved: bool,
        confidence: f64,
    },
    ConsensusReached {
        string_id: StringId,
        result: bool,
    },
}

/// Manages the lifecycle of AI testimony agents
pub struct AgentRunner {
    node_id: NodeId,
    agents: Vec<Arc<dyn TestimonyAgent>>,
    ai_collector: Arc<AITestimonyCollector>,
    event_tx: broadcast::Sender<AgentEvent>,
    clock: parking_lot::RwLock<LamportClock>,
}

impl AgentRunner {
    pub fn new(node_id: NodeId) -> Self {
        let (event_tx, _) = broadcast::channel(1000);
        let node_bytes = *node_id.as_bytes();

        let validation_agent: Arc<dyn TestimonyAgent> =
            Arc::new(ValidationAgent::new(node_bytes, vec![0x01]));
        let compliance_agent: Arc<dyn TestimonyAgent> =
            Arc::new(ComplianceAgent::new(node_bytes, vec![0x02]));
        let insurance_agent: Arc<dyn TestimonyAgent> =
            Arc::new(InsuranceAgent::new(node_bytes, vec![0x03]));

        let ai_config = AITestimonyConfig {
            min_approvals: 2,
            min_confidence: 0.7,
            max_risk_level: RiskLevel::Medium,
            timeout_secs: 30,
        };
        let ai_collector = Arc::new(AITestimonyCollector::new(ai_config));

        // Register consensus-level agent IDs
        let agents: Vec<Arc<dyn TestimonyAgent>> =
            vec![validation_agent, compliance_agent, insurance_agent];
        for agent in &agents {
            let cid = ConsensusAgentId::new(node_bytes, &agent.agent_id().to_bytes());
            ai_collector.register_agent(cid);
        }

        Self {
            node_id,
            agents,
            ai_collector,
            event_tx,
            clock: parking_lot::RwLock::new(LamportClock::new(node_id)),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.event_tx.subscribe()
    }

    /// Dispatch a notarized transaction to all agents for testimony.
    ///
    /// Returns `true` if AI consensus was reached (enough approvals with
    /// sufficient confidence), `false` otherwise.
    pub async fn evaluate_transaction(
        &self,
        string_id: StringId,
        tx_hash: &str,
        _raw_tx: Option<&str>,
    ) -> bool {
        let mut id_bytes = [0u8; 32];
        let hash_bytes = blake3::hash(tx_hash.as_bytes());
        id_bytes.copy_from_slice(hash_bytes.as_bytes());

        let tx_request = TransactionRequest {
            id: id_bytes,
            contract_id: None,
            from: *self.node_id.as_bytes(),
            to: [0u8; 32],
            action: ActionType::ContractCall,
            parameters: HashMap::new(),
            timestamp: chrono::Utc::now().timestamp(),
        };

        let context = ValidationContext {
            timestamp: chrono::Utc::now().timestamp(),
            requester: *self.node_id.as_bytes(),
            historical_data: HashMap::new(),
            oracle_data: HashMap::new(),
            risk_score: None,
        };

        let mut last_consensus = false;

        for agent in &self.agents {
            let sm_testimony = agent.provide_testimony(&tx_request, &context).await;

            let approved = sm_testimony.decision == TestimonyDecision::Approve;
            let confidence = sm_testimony.confidence;
            let agent_type_str = format!("{:?}", agent.agent_type());

            debug!(
                "Agent {} testimony for {}: approved={}, confidence={}",
                agent_type_str, tx_hash, approved, confidence
            );

            // Build the rope_consensus::Testimony base for the AITestimony
            let mut clock = self.clock.write();
            clock.increment();
            let timestamp = clock.snapshot();
            let base = ConsensusTestimony::new(
                string_id,
                self.node_id,
                AttestationType::Existence,
                timestamp,
                0,
            );

            let consensus_agent_id =
                ConsensusAgentId::new(*self.node_id.as_bytes(), &agent.agent_id().to_bytes());

            let ai_agent_type = match agent.agent_type() {
                AgentType::Validation => AIAgentType::Validation,
                AgentType::Compliance => AIAgentType::Compliance,
                AgentType::Anomaly => AIAgentType::Anomaly,
                AgentType::Contract => AIAgentType::Contract,
                _ => AIAgentType::Validation,
            };

            let verdict = match sm_testimony.decision {
                TestimonyDecision::Approve => SemanticVerdict::Approve,
                TestimonyDecision::Reject => SemanticVerdict::Reject {
                    reason: sm_testimony.reasoning.clone(),
                },
                TestimonyDecision::Abstain => SemanticVerdict::Abstain,
                TestimonyDecision::NeedsMoreInfo => SemanticVerdict::NeedsMoreInfo {
                    required: vec![sm_testimony.reasoning.clone()],
                },
            };

            let ai_testimony =
                AITestimony::new(base, consensus_agent_id, ai_agent_type, verdict, confidence);

            last_consensus = self.ai_collector.submit_testimony(ai_testimony);

            let _ = self.event_tx.send(AgentEvent::TestimonyProvided {
                agent_type: agent_type_str,
                string_id,
                approved,
                confidence,
            });
        }

        let _ = self.event_tx.send(AgentEvent::ConsensusReached {
            string_id,
            result: last_consensus,
        });

        if last_consensus {
            debug!(
                "AI consensus reached for string {}",
                hex::encode(&string_id.as_bytes()[..8])
            );
        } else {
            warn!(
                "AI consensus NOT reached for string {}",
                hex::encode(&string_id.as_bytes()[..8])
            );
        }

        last_consensus
    }

    pub fn active_agent_count(&self) -> usize {
        self.agents.len()
    }
}
