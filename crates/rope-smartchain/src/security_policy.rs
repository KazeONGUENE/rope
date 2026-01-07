//! # Security Policy Engine
//! 
//! Adaptive security policies that govern:
//! - Who can invoke what tools
//! - Risk-based access control
//! - Rate limiting and fraud prevention
//! - Compliance requirements

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use parking_lot::RwLock;

use super::tool_registry::*;

/// Main security policy engine
pub struct SecurityPolicy {
    /// Per-entity permissions
    entity_permissions: RwLock<HashMap<[u8; 32], EntityPermissions>>,
    
    /// Global policy rules
    global_rules: RwLock<Vec<PolicyRule>>,
    
    /// Rate limit tracking
    rate_limits: RwLock<HashMap<[u8; 32], RateLimitState>>,
    
    /// Blocked entities
    blocked: RwLock<HashSet<[u8; 32]>>,
}

impl SecurityPolicy {
    pub fn new() -> Self {
        Self {
            entity_permissions: RwLock::new(HashMap::new()),
            global_rules: RwLock::new(Vec::new()),
            rate_limits: RwLock::new(HashMap::new()),
            blocked: RwLock::new(HashSet::new()),
        }
    }
    
    /// Check if an entity can execute an action
    pub fn can_execute(&self, entity_id: &[u8; 32], action: &ToolAction) -> bool {
        // Check if blocked
        if self.blocked.read().contains(entity_id) {
            return false;
        }
        
        // Check rate limits
        if !self.check_rate_limit(entity_id) {
            return false;
        }
        
        // Check entity permissions
        if let Some(perms) = self.entity_permissions.read().get(entity_id) {
            if !self.check_permission(perms, action) {
                return false;
            }
        }
        
        // Check global rules
        for rule in self.global_rules.read().iter() {
            if !self.evaluate_rule(rule, entity_id, action) {
                return false;
            }
        }
        
        true
    }
    
    /// Check rate limits for an entity
    fn check_rate_limit(&self, entity_id: &[u8; 32]) -> bool {
        let now = chrono::Utc::now().timestamp();
        let mut limits = self.rate_limits.write();
        
        let state = limits.entry(*entity_id).or_insert(RateLimitState {
            window_start: now,
            request_count: 0,
        });
        
        // Reset window if expired (1 minute window)
        if now - state.window_start > 60 {
            state.window_start = now;
            state.request_count = 0;
        }
        
        // Check limit (100 requests per minute default)
        if state.request_count >= 100 {
            return false;
        }
        
        state.request_count += 1;
        true
    }
    
    /// Check if entity has permission for action
    fn check_permission(&self, perms: &EntityPermissions, action: &ToolAction) -> bool {
        // Check action type is allowed
        let action_allowed = match &action.action_type {
            ToolActionType::Transfer { .. } => perms.can_transfer,
            ToolActionType::ContractCall { .. } => perms.can_call_contracts,
            ToolActionType::Query { .. } => perms.can_query,
            ToolActionType::TokenOperation { .. } => perms.can_token_ops,
            _ => perms.custom_allowed.contains(&format!("{:?}", action.action_type)),
        };
        
        if !action_allowed {
            return false;
        }
        
        // Check value limits
        if let ToolActionType::Transfer { amount, .. } = &action.action_type {
            if let Ok(value) = amount.parse::<f64>() {
                if value > perms.max_transfer_value {
                    return false;
                }
            }
        }
        
        true
    }
    
    /// Evaluate a policy rule
    fn evaluate_rule(&self, rule: &PolicyRule, _entity_id: &[u8; 32], action: &ToolAction) -> bool {
        match rule {
            PolicyRule::RequireKyc { for_value_above } => {
                if let ToolActionType::Transfer { amount, .. } = &action.action_type {
                    if let Ok(value) = amount.parse::<f64>() {
                        if value > *for_value_above {
                            // Would need to check KYC status
                            return true; // Placeholder
                        }
                    }
                }
                true
            }
            PolicyRule::BlockProtocol { protocol: _ } => {
                // Check if action targets blocked protocol
                true // Placeholder
            }
            PolicyRule::RequireMultiSig { threshold: _ } => {
                // Check if multi-sig is present
                true // Placeholder
            }
            PolicyRule::TimeRestriction { start_hour, end_hour } => {
                let hour = chrono::Utc::now().hour();
                hour >= *start_hour && hour <= *end_hour
            }
            PolicyRule::GeoRestriction { allowed_regions: _ } => {
                // Would need to check entity's region
                true // Placeholder
            }
        }
    }
    
    /// Set permissions for an entity
    pub fn set_permissions(&self, entity_id: [u8; 32], permissions: EntityPermissions) {
        self.entity_permissions.write().insert(entity_id, permissions);
    }
    
    /// Add a global policy rule
    pub fn add_rule(&self, rule: PolicyRule) {
        self.global_rules.write().push(rule);
    }
    
    /// Block an entity
    pub fn block_entity(&self, entity_id: [u8; 32]) {
        self.blocked.write().insert(entity_id);
    }
    
    /// Unblock an entity
    pub fn unblock_entity(&self, entity_id: &[u8; 32]) {
        self.blocked.write().remove(entity_id);
    }
    
    /// Get entity's current permissions
    pub fn get_permissions(&self, entity_id: &[u8; 32]) -> Option<EntityPermissions> {
        self.entity_permissions.read().get(entity_id).cloned()
    }
}

impl Default for SecurityPolicy {
    fn default() -> Self {
        let policy = Self::new();
        
        // Add default rules
        policy.add_rule(PolicyRule::RequireKyc { for_value_above: 10000.0 });
        
        policy
    }
}

/// Permissions for an entity
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EntityPermissions {
    /// Can make transfers
    pub can_transfer: bool,
    /// Can call smart contracts
    pub can_call_contracts: bool,
    /// Can query data
    pub can_query: bool,
    /// Can perform token operations
    pub can_token_ops: bool,
    /// Maximum transfer value
    pub max_transfer_value: f64,
    /// Daily transfer limit
    pub daily_limit: f64,
    /// Custom allowed actions
    pub custom_allowed: HashSet<String>,
    /// Required approval count for high-value
    pub multi_sig_threshold: Option<u32>,
}

impl Default for EntityPermissions {
    fn default() -> Self {
        Self {
            can_transfer: true,
            can_call_contracts: true,
            can_query: true,
            can_token_ops: false,
            max_transfer_value: 100000.0,
            daily_limit: 1000000.0,
            custom_allowed: HashSet::new(),
            multi_sig_threshold: None,
        }
    }
}

/// Policy rules
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum PolicyRule {
    /// Require KYC for transfers above value
    RequireKyc { for_value_above: f64 },
    /// Block specific protocol
    BlockProtocol { protocol: String },
    /// Require multi-signature
    RequireMultiSig { threshold: u32 },
    /// Time-based restrictions
    TimeRestriction { start_hour: u32, end_hour: u32 },
    /// Geographic restrictions
    GeoRestriction { allowed_regions: Vec<String> },
}

/// Rate limit state tracking
#[derive(Clone, Debug)]
struct RateLimitState {
    window_start: i64,
    request_count: u32,
}

/// Risk assessment for an action
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RiskAssessment {
    /// Overall risk score (0-100)
    pub risk_score: u8,
    /// Risk factors
    pub factors: Vec<RiskFactor>,
    /// Recommended action
    pub recommendation: RiskRecommendation,
}

/// Risk factors
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RiskFactor {
    HighValue { amount: f64 },
    UnusualTime,
    NewEntity,
    UnknownDestination,
    HighFrequency,
    GeoAnomaly,
    PatternMatch { pattern: String },
}

/// Risk recommendations
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RiskRecommendation {
    Allow,
    RequireAdditionalVerification,
    RequireMultiSig,
    Delay,
    Block,
}

use chrono::Timelike;

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_default_permissions() {
        let perms = EntityPermissions::default();
        
        assert!(perms.can_transfer);
        assert!(perms.can_query);
        assert!(!perms.can_token_ops);
    }
    
    #[test]
    fn test_security_policy() {
        let policy = SecurityPolicy::default();
        let entity_id = [1u8; 32];
        
        // Set permissions
        policy.set_permissions(entity_id, EntityPermissions::default());
        
        // Test action
        let action = ToolAction {
            id: [0u8; 32],
            action_type: ToolActionType::Query { query_type: "balance".to_string() },
            from: entity_id,
            to: "".to_string(),
            parameters: HashMap::new(),
            contract_ref: None,
            priority: ActionPriority::Normal,
            timeout_secs: 30,
        };
        
        assert!(policy.can_execute(&entity_id, &action));
    }
    
    #[test]
    fn test_blocked_entity() {
        let policy = SecurityPolicy::default();
        let entity_id = [2u8; 32];
        
        policy.block_entity(entity_id);
        
        let action = ToolAction {
            id: [0u8; 32],
            action_type: ToolActionType::Query { query_type: "test".to_string() },
            from: entity_id,
            to: "".to_string(),
            parameters: HashMap::new(),
            contract_ref: None,
            priority: ActionPriority::Normal,
            timeout_secs: 30,
        };
        
        assert!(!policy.can_execute(&entity_id, &action));
    }
}

