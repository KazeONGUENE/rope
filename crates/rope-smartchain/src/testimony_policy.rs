//! Testimony Policy
//!
//! Defines testimony requirements based on action classification.
//! Different action types require different levels of validation.

use rope_consensus::{AIAgentType, AuditScope, RiskLevel};
use serde::{Deserialize, Serialize};

/// Testimony requirements by action classification
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestimonyPolicy {
    /// Action classification
    pub action_classification: ActionClassification,

    /// Required agent types
    pub required_agents: Vec<AIAgentType>,

    /// Minimum approvals needed
    pub min_approvals: u32,

    /// Minimum confidence score (0.0 - 1.0)
    pub min_confidence: f64,

    /// Maximum acceptable risk level
    pub max_risk_level: RiskLevel,

    /// Timeout in seconds
    pub timeout_secs: u64,

    /// Allow conditional approvals
    pub allow_conditional: bool,

    /// Require unanimous approval (no rejections)
    pub require_unanimous: bool,
}

/// Action classification
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionClassification {
    /// Read-only queries (low risk)
    Informational,

    /// State changes with low value
    LowValue { max_usd: u64 },

    /// Standard transactions
    Standard { max_usd: u64 },

    /// High-value transactions
    HighValue { min_usd: u64 },

    /// Critical operations (governance, minting)
    Critical,

    /// Custom classification
    Custom { name: String },
}

impl Default for TestimonyPolicy {
    fn default() -> Self {
        Self::standard()
    }
}

impl TestimonyPolicy {
    /// Informational queries - minimal validation
    pub fn informational() -> Self {
        Self {
            action_classification: ActionClassification::Informational,
            required_agents: vec![AIAgentType::Validation],
            min_approvals: 1,
            min_confidence: 0.5,
            max_risk_level: RiskLevel::Low,
            timeout_secs: 5,
            allow_conditional: false,
            require_unanimous: false,
        }
    }

    /// Low value transactions ($0-$100)
    pub fn low_value() -> Self {
        Self {
            action_classification: ActionClassification::LowValue { max_usd: 100 },
            required_agents: vec![AIAgentType::Validation, AIAgentType::Anomaly],
            min_approvals: 2,
            min_confidence: 0.7,
            max_risk_level: RiskLevel::Medium,
            timeout_secs: 10,
            allow_conditional: true,
            require_unanimous: false,
        }
    }

    /// Standard transactions ($100-$10,000)
    pub fn standard() -> Self {
        Self {
            action_classification: ActionClassification::Standard { max_usd: 10_000 },
            required_agents: vec![
                AIAgentType::Validation,
                AIAgentType::Compliance,
                AIAgentType::Anomaly,
            ],
            min_approvals: 3,
            min_confidence: 0.8,
            max_risk_level: RiskLevel::Medium,
            timeout_secs: 30,
            allow_conditional: true,
            require_unanimous: false,
        }
    }

    /// High value transactions ($10,000+)
    pub fn high_value() -> Self {
        Self {
            action_classification: ActionClassification::HighValue { min_usd: 10_000 },
            required_agents: vec![
                AIAgentType::Validation,
                AIAgentType::Compliance,
                AIAgentType::Anomaly,
                AIAgentType::Audit {
                    audit_scope: AuditScope::Financial,
                },
            ],
            min_approvals: 5,
            min_confidence: 0.9,
            max_risk_level: RiskLevel::Low,
            timeout_secs: 60,
            allow_conditional: true,
            require_unanimous: false,
        }
    }

    /// Critical operations (governance, token minting)
    pub fn critical() -> Self {
        Self {
            action_classification: ActionClassification::Critical,
            required_agents: vec![
                AIAgentType::Validation,
                AIAgentType::Compliance,
                AIAgentType::Anomaly,
                AIAgentType::Audit {
                    audit_scope: AuditScope::Full,
                },
                AIAgentType::Contract,
            ],
            min_approvals: 7,
            min_confidence: 0.95,
            max_risk_level: RiskLevel::Low,
            timeout_secs: 300,
            allow_conditional: false,
            require_unanimous: true,
        }
    }

    /// Get policy for a given USD value
    pub fn for_value(value_usd: u64) -> Self {
        match value_usd {
            0..=100 => Self::low_value(),
            101..=10_000 => Self::standard(),
            _ => Self::high_value(),
        }
    }

    /// Check if agents satisfy requirements
    pub fn agents_satisfy_requirements(&self, agent_types: &[AIAgentType]) -> bool {
        for required in &self.required_agents {
            if !agent_types
                .iter()
                .any(|a| Self::agent_type_matches(a, required))
            {
                return false;
            }
        }
        true
    }

    /// Check if agent types match
    fn agent_type_matches(actual: &AIAgentType, required: &AIAgentType) -> bool {
        match (actual, required) {
            (AIAgentType::Validation, AIAgentType::Validation) => true,
            (AIAgentType::Contract, AIAgentType::Contract) => true,
            (AIAgentType::Anomaly, AIAgentType::Anomaly) => true,
            (AIAgentType::Compliance, AIAgentType::Compliance) => true,
            (AIAgentType::Oracle { .. }, AIAgentType::Oracle { .. }) => true,
            (AIAgentType::Execution { .. }, AIAgentType::Execution { .. }) => true,
            (AIAgentType::Audit { .. }, AIAgentType::Audit { .. }) => true,
            (AIAgentType::Personal { .. }, AIAgentType::Personal { .. }) => true,
            (AIAgentType::Insurance { .. }, AIAgentType::Insurance { .. }) => true,
            (AIAgentType::Custom(a), AIAgentType::Custom(b)) => a == b,
            _ => false,
        }
    }

    /// Validate consensus result against policy
    pub fn validate_consensus(
        &self,
        approvals: u32,
        rejections: u32,
        avg_confidence: f64,
        max_risk: &RiskLevel,
    ) -> PolicyValidationResult {
        // Check minimum approvals
        if approvals < self.min_approvals {
            return PolicyValidationResult::InsufficientApprovals {
                got: approvals,
                needed: self.min_approvals,
            };
        }

        // Check confidence
        if avg_confidence < self.min_confidence {
            return PolicyValidationResult::InsufficientConfidence {
                got: avg_confidence,
                needed: self.min_confidence,
            };
        }

        // Check risk level
        if max_risk.as_u8() > self.max_risk_level.as_u8() {
            return PolicyValidationResult::RiskTooHigh {
                got: max_risk.clone(),
                max: self.max_risk_level.clone(),
            };
        }

        // Check unanimous requirement
        if self.require_unanimous && rejections > 0 {
            return PolicyValidationResult::NotUnanimous { rejections };
        }

        PolicyValidationResult::Valid
    }
}

/// Policy validation result
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyValidationResult {
    /// Policy requirements met
    Valid,

    /// Not enough approvals
    InsufficientApprovals { got: u32, needed: u32 },

    /// Confidence too low
    InsufficientConfidence { got: f64, needed: f64 },

    /// Risk level too high
    RiskTooHigh { got: RiskLevel, max: RiskLevel },

    /// Required unanimous but got rejections
    NotUnanimous { rejections: u32 },

    /// Missing required agent type
    MissingAgentType { agent_type: AIAgentType },
}

impl PolicyValidationResult {
    /// Check if valid
    pub fn is_valid(&self) -> bool {
        matches!(self, PolicyValidationResult::Valid)
    }
}

/// Policy registry for different action types
pub struct PolicyRegistry {
    /// Policies by classification name
    policies: std::collections::HashMap<String, TestimonyPolicy>,

    /// Default policy
    default_policy: TestimonyPolicy,
}

impl PolicyRegistry {
    /// Create new registry with standard policies
    pub fn new() -> Self {
        let mut policies = std::collections::HashMap::new();

        policies.insert(
            "informational".to_string(),
            TestimonyPolicy::informational(),
        );
        policies.insert("low_value".to_string(), TestimonyPolicy::low_value());
        policies.insert("standard".to_string(), TestimonyPolicy::standard());
        policies.insert("high_value".to_string(), TestimonyPolicy::high_value());
        policies.insert("critical".to_string(), TestimonyPolicy::critical());

        Self {
            policies,
            default_policy: TestimonyPolicy::standard(),
        }
    }

    /// Register a custom policy
    pub fn register(&mut self, name: &str, policy: TestimonyPolicy) {
        self.policies.insert(name.to_string(), policy);
    }

    /// Get policy by name
    pub fn get(&self, name: &str) -> Option<&TestimonyPolicy> {
        self.policies.get(name)
    }

    /// Get policy for action type
    pub fn get_for_action(&self, action_type: &str, value_usd: Option<u64>) -> &TestimonyPolicy {
        // Check for specific action type policy
        if let Some(policy) = self.policies.get(action_type) {
            return policy;
        }

        // Fall back to value-based policy
        if let Some(value) = value_usd {
            return match value {
                0..=100 => self
                    .policies
                    .get("low_value")
                    .unwrap_or(&self.default_policy),
                101..=10_000 => self
                    .policies
                    .get("standard")
                    .unwrap_or(&self.default_policy),
                _ => self
                    .policies
                    .get("high_value")
                    .unwrap_or(&self.default_policy),
            };
        }

        &self.default_policy
    }

    /// Set default policy
    pub fn set_default(&mut self, policy: TestimonyPolicy) {
        self.default_policy = policy;
    }
}

impl Default for PolicyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_policy_for_value() {
        let low = TestimonyPolicy::for_value(50);
        assert!(matches!(
            low.action_classification,
            ActionClassification::LowValue { .. }
        ));

        let standard = TestimonyPolicy::for_value(5000);
        assert!(matches!(
            standard.action_classification,
            ActionClassification::Standard { .. }
        ));

        let high = TestimonyPolicy::for_value(50_000);
        assert!(matches!(
            high.action_classification,
            ActionClassification::HighValue { .. }
        ));
    }

    #[test]
    fn test_validate_consensus() {
        let policy = TestimonyPolicy::standard();

        // Valid
        let result = policy.validate_consensus(3, 0, 0.85, &RiskLevel::Low);
        assert!(result.is_valid());

        // Insufficient approvals
        let result = policy.validate_consensus(2, 0, 0.85, &RiskLevel::Low);
        assert!(matches!(
            result,
            PolicyValidationResult::InsufficientApprovals { .. }
        ));

        // Insufficient confidence
        let result = policy.validate_consensus(3, 0, 0.5, &RiskLevel::Low);
        assert!(matches!(
            result,
            PolicyValidationResult::InsufficientConfidence { .. }
        ));

        // Risk too high
        let result = policy.validate_consensus(3, 0, 0.85, &RiskLevel::Critical);
        assert!(matches!(result, PolicyValidationResult::RiskTooHigh { .. }));
    }

    #[test]
    fn test_critical_unanimous() {
        let policy = TestimonyPolicy::critical();

        // Unanimous approval
        let result = policy.validate_consensus(7, 0, 0.98, &RiskLevel::Low);
        assert!(result.is_valid());

        // Has rejection
        let result = policy.validate_consensus(7, 1, 0.98, &RiskLevel::Low);
        assert!(matches!(
            result,
            PolicyValidationResult::NotUnanimous { .. }
        ));
    }

    #[test]
    fn test_policy_registry() {
        let registry = PolicyRegistry::new();

        let policy = registry.get_for_action("transfer", Some(5000));
        assert_eq!(policy.min_approvals, 3); // Standard

        let policy = registry.get_for_action("critical", None);
        assert_eq!(policy.min_approvals, 7); // Critical
    }
}
