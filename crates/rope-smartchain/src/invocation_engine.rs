//! # Invocation Engine
//!
//! The core engine that orchestrates AI testimony validation and tool execution.
//! This is the "brain" of the Smartchain that:
//!
//! 1. Receives transaction/contract requests
//! 2. Routes to appropriate AI testimony agents for validation
//! 3. Aggregates testimonies and checks thresholds
//! 4. Invokes vetted tools to execute actions
//! 5. Records results in the String Lattice

// Invocation engine for executing vetted tools
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use super::security_policy::*;
use super::testimony_agent::*;
use super::tool_registry::*;

/// The main invocation engine
pub struct InvocationEngine {
    /// Registry of AI testimony agents
    agents: RwLock<HashMap<[u8; 32], Arc<dyn TestimonyAgent>>>,

    /// Registry of vetted tools
    tool_registry: Arc<ToolRegistry>,

    /// Security policy engine
    security_policy: Arc<SecurityPolicy>,

    /// Pending invocations
    pending: RwLock<HashMap<[u8; 32], InvocationState>>,

    /// Completed invocations (for audit)
    completed: RwLock<Vec<InvocationRecord>>,
}

impl InvocationEngine {
    pub fn new(tool_registry: Arc<ToolRegistry>, security_policy: Arc<SecurityPolicy>) -> Self {
        Self {
            agents: RwLock::new(HashMap::new()),
            tool_registry,
            security_policy,
            pending: RwLock::new(HashMap::new()),
            completed: RwLock::new(Vec::new()),
        }
    }

    /// Register an AI testimony agent
    pub fn register_agent(&self, agent: Arc<dyn TestimonyAgent>) {
        let id = agent.agent_id().to_bytes();
        self.agents.write().insert(id, agent);
    }

    /// Process a contract for execution
    pub async fn process_contract(
        &self,
        contract: &DigitizedContract,
    ) -> Result<InvocationResult, InvocationError> {
        let invocation_id = *blake3::hash(&contract.contract_id).as_bytes();

        // 1. Initialize invocation state
        let state = InvocationState {
            invocation_id,
            contract_id: contract.contract_id,
            phase: InvocationPhase::ValidatingConditions,
            testimonies: Vec::new(),
            execution_results: Vec::new(),
            started_at: chrono::Utc::now().timestamp(),
            completed_at: None,
        };
        self.pending.write().insert(invocation_id, state);

        // 2. Validate all conditions with AI agents
        let mut all_conditions_met = true;
        let mut condition_results = Vec::new();

        for condition in &contract.conditions {
            let result = self.validate_condition(condition, contract).await?;
            if !result.satisfied {
                all_conditions_met = false;
            }
            condition_results.push(result);
        }

        // Update phase
        if let Some(state) = self.pending.write().get_mut(&invocation_id) {
            state.phase = if all_conditions_met {
                InvocationPhase::ExecutingActions
            } else {
                InvocationPhase::ConditionsNotMet
            };
        }

        // 3. If conditions met, execute actions
        let mut action_results = Vec::new();
        if all_conditions_met {
            for action in &contract.actions {
                let result = self.execute_action(action, contract).await?;
                action_results.push(result);
            }
        }

        // 4. Finalize
        let now = chrono::Utc::now().timestamp();
        let final_status = if all_conditions_met && action_results.iter().all(|r| r.success) {
            InvocationStatus::Completed
        } else if !all_conditions_met {
            InvocationStatus::ConditionsNotMet
        } else {
            InvocationStatus::PartialFailure
        };

        if let Some(state) = self.pending.write().get_mut(&invocation_id) {
            state.phase = InvocationPhase::Completed;
            state.completed_at = Some(now);
        }

        // Record for audit
        let record = InvocationRecord {
            invocation_id,
            contract_id: contract.contract_id,
            status: final_status.clone(),
            condition_results: condition_results.clone(),
            action_results: action_results.clone(),
            started_at: self
                .pending
                .read()
                .get(&invocation_id)
                .map(|s| s.started_at)
                .unwrap_or(now),
            completed_at: now,
        };
        self.completed.write().push(record);

        // Remove from pending
        self.pending.write().remove(&invocation_id);

        Ok(InvocationResult {
            invocation_id,
            status: final_status,
            condition_results,
            action_results,
        })
    }

    /// Validate a single condition using AI agents
    async fn validate_condition(
        &self,
        condition: &ContractCondition,
        contract: &DigitizedContract,
    ) -> Result<ValidationResult, InvocationError> {
        let agents = self.agents.read();

        // Find suitable agents for this condition
        let suitable_agents: Vec<_> = agents
            .values()
            .filter(|agent| condition.required_agents.contains(&agent.agent_type()))
            .collect();

        if suitable_agents.is_empty() {
            return Err(InvocationError::NoSuitableAgents);
        }

        // Create validation context
        let context = ValidationContext {
            timestamp: chrono::Utc::now().timestamp(),
            requester: contract
                .parties
                .first()
                .map(|p| p.node_id)
                .unwrap_or([0u8; 32]),
            historical_data: HashMap::new(),
            oracle_data: HashMap::new(),
            risk_score: None,
        };

        // Collect testimonies from all agents
        let mut approvals = 0u32;
        let mut total_confidence = 0.0f64;

        for agent in &suitable_agents {
            let result = agent.validate_condition(condition, &context).await;
            if result.satisfied {
                approvals += 1;
                total_confidence += result.confidence;
            }
        }

        let approval_rate = approvals as f64 / suitable_agents.len() as f64;
        let satisfied = approval_rate >= condition.approval_threshold;
        let avg_confidence = if approvals > 0 {
            total_confidence / approvals as f64
        } else {
            0.0
        };

        Ok(ValidationResult {
            satisfied,
            confidence: avg_confidence,
            reason: if satisfied {
                format!("{}/{} agents approved", approvals, suitable_agents.len())
            } else {
                format!(
                    "Threshold not met: {:.1}% < {:.1}%",
                    approval_rate * 100.0,
                    condition.approval_threshold * 100.0
                )
            },
            evidence: Vec::new(),
            signature: Vec::new(),
        })
    }

    /// Execute a single action using vetted tools
    async fn execute_action(
        &self,
        action: &ContractAction,
        contract: &DigitizedContract,
    ) -> Result<ExecutionResult, InvocationError> {
        // Convert contract action to tool action
        let tool_action = self.convert_to_tool_action(action, contract)?;

        // Find the best tool for this action
        let tool = self
            .tool_registry
            .find_best_tool_for_action(&tool_action)
            .ok_or(InvocationError::NoSuitableTool)?;

        // Check security policy
        let caller = contract
            .parties
            .first()
            .map(|p| p.node_id)
            .unwrap_or([0u8; 32]);
        if !self.security_policy.can_execute(&caller, &tool_action) {
            return Err(InvocationError::SecurityPolicyViolation);
        }

        // Create execution context with testimony signatures
        let context = ExecutionContext {
            caller,
            timestamp: chrono::Utc::now().timestamp(),
            fee_budget: None,
            testimony_signatures: Vec::new(),
            metadata: HashMap::new(),
        };

        // Execute via tool
        let result = tool.execute(&tool_action, &context).await;

        Ok(result)
    }

    /// Convert contract action to tool action
    fn convert_to_tool_action(
        &self,
        action: &ContractAction,
        contract: &DigitizedContract,
    ) -> Result<ToolAction, InvocationError> {
        let action_type = match &action.action_type {
            ActionType::Payment => ToolActionType::Transfer {
                asset: action
                    .parameters
                    .get("asset")
                    .and_then(|v| {
                        if let ConditionValue::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "USD".to_string()),
                amount: action
                    .parameters
                    .get("amount")
                    .and_then(|v| {
                        if let ConditionValue::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| "0".to_string()),
            },
            ActionType::AssetTransfer => ToolActionType::Transfer {
                asset: action
                    .parameters
                    .get("asset")
                    .and_then(|v| {
                        if let ConditionValue::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
                amount: "1".to_string(),
            },
            ActionType::ContractCall => ToolActionType::ContractCall {
                method: action
                    .parameters
                    .get("method")
                    .and_then(|v| {
                        if let ConditionValue::String(s) = v {
                            Some(s.clone())
                        } else {
                            None
                        }
                    })
                    .unwrap_or_default(),
            },
            ActionType::TokenOperation => ToolActionType::TokenOperation { op: TokenOp::Mint },
            _ => ToolActionType::Custom("unknown".to_string()),
        };

        let to = match &action.target_protocol {
            TargetProtocol::Ethereum { contract, .. } => contract.clone(),
            TargetProtocol::Banking { account, .. } => account.clone(),
            _ => "".to_string(),
        };

        Ok(ToolAction {
            id: action.id,
            action_type,
            from: contract
                .parties
                .first()
                .map(|p| p.node_id)
                .unwrap_or([0u8; 32]),
            to,
            parameters: HashMap::new(),
            contract_ref: Some(contract.contract_id),
            priority: ActionPriority::Normal,
            timeout_secs: 120,
        })
    }

    /// Get status of a pending invocation
    pub fn get_status(&self, invocation_id: &[u8; 32]) -> Option<InvocationPhase> {
        self.pending
            .read()
            .get(invocation_id)
            .map(|s| s.phase.clone())
    }

    /// Get completed invocation record
    pub fn get_record(&self, invocation_id: &[u8; 32]) -> Option<InvocationRecord> {
        self.completed
            .read()
            .iter()
            .find(|r| &r.invocation_id == invocation_id)
            .cloned()
    }
}

/// State of an invocation
#[derive(Clone, Debug)]
pub struct InvocationState {
    pub invocation_id: [u8; 32],
    pub contract_id: [u8; 32],
    pub phase: InvocationPhase,
    pub testimonies: Vec<Testimony>,
    pub execution_results: Vec<ExecutionResult>,
    pub started_at: i64,
    pub completed_at: Option<i64>,
}

/// Invocation phases
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InvocationPhase {
    /// Validating contract conditions with AI agents
    ValidatingConditions,
    /// Conditions not met
    ConditionsNotMet,
    /// Executing actions via vetted tools
    ExecutingActions,
    /// All done
    Completed,
    /// Failed
    Failed(String),
}

/// Final invocation result
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvocationResult {
    pub invocation_id: [u8; 32],
    pub status: InvocationStatus,
    pub condition_results: Vec<ValidationResult>,
    pub action_results: Vec<ExecutionResult>,
}

/// Invocation status
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InvocationStatus {
    Completed,
    ConditionsNotMet,
    PartialFailure,
    Failed,
}

/// Record of completed invocation (for audit)
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InvocationRecord {
    pub invocation_id: [u8; 32],
    pub contract_id: [u8; 32],
    pub status: InvocationStatus,
    pub condition_results: Vec<ValidationResult>,
    pub action_results: Vec<ExecutionResult>,
    pub started_at: i64,
    pub completed_at: i64,
}

/// Invocation errors
#[derive(Clone, Debug)]
pub enum InvocationError {
    NoSuitableAgents,
    NoSuitableTool,
    SecurityPolicyViolation,
    ValidationFailed(String),
    ExecutionFailed(String),
    Timeout,
}

impl std::fmt::Display for InvocationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InvocationError::NoSuitableAgents => write!(f, "No suitable AI agents found"),
            InvocationError::NoSuitableTool => write!(f, "No suitable vetted tool found"),
            InvocationError::SecurityPolicyViolation => write!(f, "Security policy violation"),
            InvocationError::ValidationFailed(s) => write!(f, "Validation failed: {}", s),
            InvocationError::ExecutionFailed(s) => write!(f, "Execution failed: {}", s),
            InvocationError::Timeout => write!(f, "Operation timed out"),
        }
    }
}

impl std::error::Error for InvocationError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invocation_engine_creation() {
        let registry = Arc::new(ToolRegistry::new());
        let policy = Arc::new(SecurityPolicy::default());
        let engine = InvocationEngine::new(registry, policy);

        // Should start with no agents
        assert!(engine.agents.read().is_empty());
    }
}
