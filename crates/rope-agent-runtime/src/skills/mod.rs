//! Skill System
//!
//! ClawHub-style skill marketplace with governance approval.

use crate::agents::PersonalCapability;
use crate::error::SkillError;
use crate::identity::DatawalletIdentity;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Skill ID type
pub type SkillId = [u8; 32];

/// A skill definition
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skill {
    /// Unique skill ID (hash of content)
    pub id: SkillId,

    /// Human-readable name
    pub name: String,

    /// Description
    pub description: String,

    /// Version (semver)
    pub version: String,

    /// Developer/author
    pub author: String,

    /// Required capabilities
    pub required_capabilities: Vec<PersonalCapability>,

    /// Skill instructions
    pub instructions: SkillInstructions,

    /// Required tools
    pub required_tools: Vec<String>,

    /// Governance approval status
    pub governance: GovernanceStatus,

    /// Security audit
    pub audit: Option<SecurityAudit>,

    /// Usage statistics
    pub stats: SkillStats,
}

impl Skill {
    /// Create new skill
    pub fn new(name: String, description: String, instructions: SkillInstructions) -> Self {
        let id = Self::compute_id(&name, &description, &instructions);

        Self {
            id,
            name,
            description,
            version: "1.0.0".to_string(),
            author: "unknown".to_string(),
            required_capabilities: Vec::new(),
            instructions,
            required_tools: Vec::new(),
            governance: GovernanceStatus::default(),
            audit: None,
            stats: SkillStats::default(),
        }
    }

    /// Compute skill ID from content
    fn compute_id(name: &str, description: &str, instructions: &SkillInstructions) -> SkillId {
        let mut data = Vec::new();
        data.extend_from_slice(name.as_bytes());
        data.extend_from_slice(description.as_bytes());
        data.extend_from_slice(instructions.system_prompt.as_bytes());

        *blake3::hash(&data).as_bytes()
    }

    /// Check if skill is approved for use
    pub fn is_approved(&self) -> bool {
        self.governance.approved && !self.governance.suspended
    }

    /// Verify governance approval
    pub fn verify_governance_approval(&self) -> Result<(), SkillError> {
        if !self.governance.approved {
            return Err(SkillError::NotApproved);
        }
        if self.governance.suspended {
            return Err(SkillError::Suspended(self.governance.suspension_reason.clone()));
        }
        Ok(())
    }

    /// Verify security audit
    pub fn verify_security_audit(&self) -> Result<(), SkillError> {
        if let Some(audit) = &self.audit {
            if audit.score < 70 {
                return Err(SkillError::ValidationFailed(
                    format!("Audit score too low: {}", audit.score)
                ));
            }
            Ok(())
        } else {
            // Skills without audit are allowed but warned
            Ok(())
        }
    }
}

/// Skill instructions
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillInstructions {
    /// System prompt for AI model
    pub system_prompt: String,

    /// Example conversations
    pub examples: Vec<ConversationExample>,

    /// Trigger patterns (regex/keywords)
    pub triggers: Vec<TriggerPattern>,

    /// Action templates
    pub action_templates: Vec<ActionTemplate>,

    /// Validation rules
    pub validation_rules: Vec<ValidationRule>,
}

impl Default for SkillInstructions {
    fn default() -> Self {
        Self {
            system_prompt: String::new(),
            examples: Vec::new(),
            triggers: Vec::new(),
            action_templates: Vec::new(),
            validation_rules: Vec::new(),
        }
    }
}

/// Conversation example for skill
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConversationExample {
    /// User input
    pub user: String,
    /// Assistant response
    pub assistant: String,
}

/// Trigger pattern for skill activation
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TriggerPattern {
    /// Pattern type
    pub pattern_type: PatternType,
    /// Pattern value
    pub pattern: String,
    /// Priority (higher = checked first)
    pub priority: u8,
}

/// Pattern types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PatternType {
    /// Exact match
    Exact,
    /// Contains substring
    Contains,
    /// Starts with
    StartsWith,
    /// Regular expression
    Regex,
    /// Keyword list
    Keywords,
}

/// Action template
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionTemplate {
    /// Template name
    pub name: String,
    /// Action type
    pub action_type: String,
    /// Template parameters
    pub parameters: HashMap<String, String>,
}

/// Validation rule
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ValidationRule {
    /// Rule name
    pub name: String,
    /// Rule type
    pub rule_type: RuleType,
    /// Rule value
    pub value: String,
}

impl ValidationRule {
    /// Validate against context
    pub fn validate(&self, _context: &HashMap<String, String>) -> bool {
        // Simplified validation
        true
    }
}

/// Rule types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuleType {
    /// Required parameter
    Required,
    /// Parameter type check
    TypeCheck,
    /// Value range
    Range,
    /// Custom expression
    Expression,
}

/// Governance status
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct GovernanceStatus {
    /// Is skill approved?
    pub approved: bool,

    /// Approval vote reference
    pub vote_string_id: Option<[u8; 32]>,

    /// Approval timestamp
    pub approved_at: Option<i64>,

    /// Approving governors
    pub approvers: Vec<[u8; 32]>,

    /// Is skill suspended?
    pub suspended: bool,

    /// Suspension reason
    pub suspension_reason: Option<String>,
}

/// Security audit
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityAudit {
    /// Auditor identity
    pub auditor: String,

    /// Audit timestamp
    pub audited_at: i64,

    /// Report hash (stored on IPFS)
    pub report_hash: [u8; 32],

    /// Security score (0-100)
    pub score: u8,

    /// Identified risks
    pub risks: Vec<SecurityRisk>,

    /// Mitigations
    pub mitigations: Vec<String>,
}

/// Security risk
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SecurityRisk {
    /// Risk category
    pub category: String,
    /// Severity (1-10)
    pub severity: u8,
    /// Description
    pub description: String,
}

/// Skill usage statistics
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SkillStats {
    /// Total executions
    pub total_executions: u64,
    /// Successful executions
    pub successful_executions: u64,
    /// Average execution time (ms)
    pub avg_execution_time_ms: u64,
    /// Last used timestamp
    pub last_used: Option<i64>,
}

/// Skill registry
pub struct SkillRegistry {
    /// Loaded skills
    skills: HashMap<SkillId, Skill>,

    /// Skill name to ID mapping
    name_index: HashMap<String, SkillId>,

    /// Execution history
    history: Vec<SkillExecution>,
}

impl SkillRegistry {
    /// Create new registry
    pub fn new() -> Self {
        Self {
            skills: HashMap::new(),
            name_index: HashMap::new(),
            history: Vec::new(),
        }
    }

    /// Register a skill
    pub fn register(&mut self, skill: Skill) {
        self.name_index.insert(skill.name.clone(), skill.id);
        self.skills.insert(skill.id, skill);
    }

    /// Get skill by ID
    pub fn get(&self, id: &SkillId) -> Option<&Skill> {
        self.skills.get(id)
    }

    /// Get skill by name
    pub fn get_by_name(&self, name: &str) -> Option<&Skill> {
        self.name_index.get(name).and_then(|id| self.skills.get(id))
    }

    /// List all skills
    pub fn list(&self) -> Vec<&Skill> {
        self.skills.values().collect()
    }

    /// Count skills
    pub fn skill_count(&self) -> usize {
        self.skills.len()
    }

    /// Unregister a skill
    pub fn unregister(&mut self, id: &SkillId) -> Option<Skill> {
        if let Some(skill) = self.skills.remove(id) {
            self.name_index.remove(&skill.name);
            Some(skill)
        } else {
            None
        }
    }

    /// Record skill execution
    pub fn record_execution(&mut self, execution: SkillExecution) {
        // Update skill stats
        if let Some(skill) = self.skills.get_mut(&execution.skill_id) {
            skill.stats.total_executions += 1;
            if execution.success {
                skill.stats.successful_executions += 1;
            }
            skill.stats.last_used = Some(execution.timestamp);
        }

        self.history.push(execution);

        // Limit history size
        if self.history.len() > 1000 {
            self.history.remove(0);
        }
    }

    /// Get execution history
    pub fn execution_history(&self, limit: usize) -> &[SkillExecution] {
        let start = self.history.len().saturating_sub(limit);
        &self.history[start..]
    }

    /// Find skills matching a trigger
    pub fn find_matching_skills(&self, text: &str) -> Vec<&Skill> {
        let lower = text.to_lowercase();

        self.skills
            .values()
            .filter(|skill| {
                skill.instructions.triggers.iter().any(|trigger| {
                    match trigger.pattern_type {
                        PatternType::Exact => lower == trigger.pattern.to_lowercase(),
                        PatternType::Contains => lower.contains(&trigger.pattern.to_lowercase()),
                        PatternType::StartsWith => lower.starts_with(&trigger.pattern.to_lowercase()),
                        PatternType::Keywords => {
                            trigger.pattern.split(',')
                                .any(|kw| lower.contains(kw.trim().to_lowercase().as_str()))
                        }
                        PatternType::Regex => {
                            // Simplified - would use regex crate in production
                            lower.contains(&trigger.pattern.to_lowercase())
                        }
                    }
                })
            })
            .collect()
    }
}

impl Default for SkillRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Skill execution record
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillExecution {
    /// Skill ID
    pub skill_id: SkillId,

    /// Execution timestamp
    pub timestamp: i64,

    /// Was successful
    pub success: bool,

    /// Execution time (ms)
    pub execution_time_ms: u64,

    /// Error message (if failed)
    pub error: Option<String>,
}

/// Skill summary for marketplace
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillSummary {
    /// Skill ID
    pub id: SkillId,

    /// Name
    pub name: String,

    /// Description
    pub description: String,

    /// Version
    pub version: String,

    /// Author
    pub author: String,

    /// Required capabilities
    pub required_capabilities: Vec<PersonalCapability>,

    /// Audit score
    pub audit_score: Option<u8>,

    /// Usage count
    pub usage_count: u64,
}

impl From<&Skill> for SkillSummary {
    fn from(skill: &Skill) -> Self {
        Self {
            id: skill.id,
            name: skill.name.clone(),
            description: skill.description.clone(),
            version: skill.version.clone(),
            author: skill.author.clone(),
            required_capabilities: skill.required_capabilities.clone(),
            audit_score: skill.audit.as_ref().map(|a| a.score),
            usage_count: skill.stats.total_executions,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_creation() {
        let skill = Skill::new(
            "Test Skill".to_string(),
            "A test skill".to_string(),
            SkillInstructions::default(),
        );

        assert!(!skill.id.iter().all(|&b| b == 0));
        assert!(!skill.is_approved()); // Not approved by default
    }

    #[test]
    fn test_skill_registry() {
        let mut registry = SkillRegistry::new();

        let skill = Skill::new(
            "Weather".to_string(),
            "Get weather info".to_string(),
            SkillInstructions::default(),
        );

        let id = skill.id;
        registry.register(skill);

        assert!(registry.get(&id).is_some());
        assert!(registry.get_by_name("Weather").is_some());
        assert_eq!(registry.skill_count(), 1);
    }

    #[test]
    fn test_skill_matching() {
        let mut registry = SkillRegistry::new();

        let mut skill = Skill::new(
            "Weather".to_string(),
            "Get weather info".to_string(),
            SkillInstructions::default(),
        );

        skill.instructions.triggers.push(TriggerPattern {
            pattern_type: PatternType::Contains,
            pattern: "weather".to_string(),
            priority: 1,
        });

        registry.register(skill);

        let matches = registry.find_matching_skills("What's the weather today?");
        assert_eq!(matches.len(), 1);
    }
}
