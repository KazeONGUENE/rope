//! Intent Parsing and Action Types
//!
//! Parses user messages into structured intents and action types
//! for Testimony consensus routing.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Parsed intent from user message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Intent {
    /// Intent type
    pub intent_type: IntentType,

    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,

    /// Extracted entities
    pub entities: HashMap<String, Entity>,

    /// Original raw text
    pub raw_text: String,

    /// Parsing timestamp
    pub parsed_at: i64,
}

impl Intent {
    /// Check if this intent requires Testimony consensus
    pub fn requires_testimony(&self) -> bool {
        matches!(
            self.intent_type,
            IntentType::Transfer { .. }
                | IntentType::Swap { .. }
                | IntentType::Stake { .. }
                | IntentType::ContractCall { .. }
                | IntentType::SkillInvocation { .. }
        )
    }

    /// Get timeout for this intent type
    pub fn timeout_secs(&self) -> u64 {
        match &self.intent_type {
            IntentType::Query { .. } | IntentType::Status { .. } | IntentType::Help => 5,
            IntentType::SendMessage { .. } | IntentType::SetReminder { .. } => 10,
            IntentType::Transfer { .. } | IntentType::Swap { .. } => 30,
            IntentType::Stake { .. } => 60,
            IntentType::ContractCall { .. } => 60,
            IntentType::SkillInvocation { .. } => 30,
        }
    }

    /// Convert to ActionType for token creation
    pub fn to_action_type(&self) -> ActionType {
        match &self.intent_type {
            IntentType::Query { .. } => ActionType::Query,
            IntentType::Status { .. } => ActionType::Query,
            IntentType::Help => ActionType::Query,
            IntentType::SendMessage { .. } => ActionType::Message,
            IntentType::SetReminder { .. } => ActionType::Reminder,
            IntentType::Transfer { asset, .. } => ActionType::Transfer {
                asset: asset.clone(),
            },
            IntentType::Swap {
                from_asset,
                to_asset,
                ..
            } => ActionType::Swap {
                from_asset: from_asset.clone(),
                to_asset: to_asset.clone(),
            },
            IntentType::Stake { .. } => ActionType::Stake,
            IntentType::ContractCall { contract, .. } => ActionType::ContractCall {
                contract: contract.clone(),
            },
            IntentType::SkillInvocation { skill_id, .. } => ActionType::SkillExecution {
                skill_id: *skill_id,
            },
        }
    }

    /// Estimate USD value for this intent
    pub fn estimated_value_usd(&self) -> Option<u64> {
        match &self.intent_type {
            IntentType::Transfer { amount, .. } => {
                // Simplified: assume 1 FAT = $0.01
                Some((*amount * 0.01) as u64)
            }
            IntentType::Swap { amount, .. } => Some((*amount * 0.01) as u64),
            IntentType::Stake { amount, .. } => Some((*amount * 0.01) as u64),
            _ => None,
        }
    }
}

/// Intent types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum IntentType {
    // === Informational (no Testimony needed) ===
    /// Query for information
    Query { topic: String },

    /// Status check
    Status { resource: String },

    /// Help request
    Help,

    // === Low-risk actions (minimal Testimony) ===
    /// Send a message on user's behalf
    SendMessage { channel: String, recipient: String },

    /// Set a reminder
    SetReminder { time: i64, message: String },

    // === Financial actions (full Testimony) ===
    /// Transfer assets
    Transfer {
        asset: String,
        amount: f64,
        recipient: String,
    },

    /// Swap assets
    Swap {
        from_asset: String,
        to_asset: String,
        amount: f64,
    },

    /// Stake assets
    Stake {
        amount: f64,
        validator: Option<String>,
    },

    // === Smart contract actions (full Testimony) ===
    /// Call a smart contract method
    ContractCall {
        contract: String,
        method: String,
        params: Vec<String>,
    },

    // === Skill invocation ===
    /// Invoke a loaded skill
    SkillInvocation {
        skill_id: [u8; 32],
        params: HashMap<String, String>,
    },
}

/// Action types for authorization tokens
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ActionType {
    /// Read-only query
    Query,

    /// Send message
    Message,

    /// Set reminder
    Reminder,

    /// Transfer assets
    Transfer { asset: String },

    /// Swap assets
    Swap {
        from_asset: String,
        to_asset: String,
    },

    /// Stake assets
    Stake,

    /// Call smart contract
    ContractCall { contract: String },

    /// Execute skill
    SkillExecution { skill_id: [u8; 32] },

    /// Any action (wildcard - use with caution)
    Any,
}

impl ActionType {
    /// Check if this action type matches another
    pub fn matches(&self, other: &ActionType) -> bool {
        match (self, other) {
            (ActionType::Any, _) | (_, ActionType::Any) => true,
            (ActionType::Transfer { asset: a1 }, ActionType::Transfer { asset: a2 }) => a1 == a2,
            (
                ActionType::Swap {
                    from_asset: f1,
                    to_asset: t1,
                },
                ActionType::Swap {
                    from_asset: f2,
                    to_asset: t2,
                },
            ) => f1 == f2 && t1 == t2,
            (
                ActionType::ContractCall { contract: c1 },
                ActionType::ContractCall { contract: c2 },
            ) => c1 == c2,
            (
                ActionType::SkillExecution { skill_id: s1 },
                ActionType::SkillExecution { skill_id: s2 },
            ) => s1 == s2,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

/// Extracted entity from user message
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entity {
    /// Entity type
    pub entity_type: EntityType,

    /// Entity value
    pub value: String,

    /// Start position in original text
    pub start: usize,

    /// End position in original text
    pub end: usize,

    /// Confidence score
    pub confidence: f64,
}

/// Entity types
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntityType {
    /// Cryptocurrency/token amount
    Amount,

    /// Asset/token name
    Asset,

    /// Wallet address
    Address,

    /// Contract address
    Contract,

    /// Date/time
    DateTime,

    /// Duration
    Duration,

    /// Person/contact name
    Person,

    /// Channel name
    Channel,

    /// Skill name
    Skill,

    /// Custom entity
    Custom(String),
}

/// Intent parser
pub struct IntentParser {
    /// Known skills for matching
    known_skills: Vec<(String, [u8; 32])>,

    /// Known assets
    known_assets: Vec<String>,
}

impl IntentParser {
    /// Create new intent parser
    pub fn new() -> Self {
        Self {
            known_skills: Vec::new(),
            known_assets: vec![
                "FAT".to_string(),
                "DC".to_string(),
                "ETH".to_string(),
                "BTC".to_string(),
                "USDT".to_string(),
                "USDC".to_string(),
            ],
        }
    }

    /// Register a skill for intent matching
    pub fn register_skill(&mut self, name: String, id: [u8; 32]) {
        self.known_skills.push((name, id));
    }

    /// Parse user message into intent
    pub fn parse(&self, message: &str) -> Intent {
        let lower = message.to_lowercase();

        // Simple pattern matching (in production, use NLP model)
        let intent_type = if lower.contains("transfer") || lower.contains("send") {
            self.parse_transfer(&lower)
        } else if lower.contains("swap") || lower.contains("exchange") {
            self.parse_swap(&lower)
        } else if lower.contains("stake") {
            self.parse_stake(&lower)
        } else if lower.contains("remind") {
            self.parse_reminder(&lower)
        } else if lower.contains("status") || lower.contains("balance") {
            IntentType::Status {
                resource: "balance".to_string(),
            }
        } else if lower.contains("help") {
            IntentType::Help
        } else {
            IntentType::Query {
                topic: message.to_string(),
            }
        };

        Intent {
            intent_type,
            confidence: 0.8, // Placeholder
            entities: HashMap::new(),
            raw_text: message.to_string(),
            parsed_at: chrono::Utc::now().timestamp(),
        }
    }

    fn parse_transfer(&self, message: &str) -> IntentType {
        // Extract amount (simplified regex-free parsing)
        let amount = self.extract_number(message).unwrap_or(0.0);

        // Extract asset
        let asset = self
            .known_assets
            .iter()
            .find(|a| message.contains(&a.to_lowercase()))
            .cloned()
            .unwrap_or_else(|| "FAT".to_string());

        // Extract recipient (simplified)
        let recipient = if message.contains("0x") {
            message
                .split_whitespace()
                .find(|w| w.starts_with("0x"))
                .unwrap_or("0x0")
                .to_string()
        } else {
            "unknown".to_string()
        };

        IntentType::Transfer {
            asset,
            amount,
            recipient,
        }
    }

    fn parse_swap(&self, message: &str) -> IntentType {
        let amount = self.extract_number(message).unwrap_or(0.0);

        // Find from/to assets
        let mut from_asset = "FAT".to_string();
        let mut to_asset = "USDT".to_string();

        for asset in &self.known_assets {
            let lower_asset = asset.to_lowercase();
            if message.contains(&format!("from {}", lower_asset))
                || message.contains(&format!("{} to", lower_asset))
            {
                from_asset = asset.clone();
            }
            if message.contains(&format!("to {}", lower_asset))
                || message.contains(&format!("for {}", lower_asset))
            {
                to_asset = asset.clone();
            }
        }

        IntentType::Swap {
            from_asset,
            to_asset,
            amount,
        }
    }

    fn parse_stake(&self, message: &str) -> IntentType {
        let amount = self.extract_number(message).unwrap_or(0.0);

        IntentType::Stake {
            amount,
            validator: None,
        }
    }

    fn parse_reminder(&self, message: &str) -> IntentType {
        // Simplified time parsing
        let time = chrono::Utc::now().timestamp() + 3600; // Default 1 hour

        IntentType::SetReminder {
            time,
            message: message.to_string(),
        }
    }

    fn extract_number(&self, message: &str) -> Option<f64> {
        for word in message.split_whitespace() {
            if let Ok(num) = word.replace(',', "").parse::<f64>() {
                return Some(num);
            }
        }
        None
    }
}

impl Default for IntentParser {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_transfer() {
        let parser = IntentParser::new();
        let intent = parser.parse("transfer 100 FAT to 0x1234567890abcdef");

        assert!(matches!(intent.intent_type, IntentType::Transfer { .. }));
        assert!(intent.requires_testimony());
    }

    #[test]
    fn test_parse_query() {
        let parser = IntentParser::new();
        let intent = parser.parse("what is the weather today?");

        assert!(matches!(intent.intent_type, IntentType::Query { .. }));
        assert!(!intent.requires_testimony());
    }

    #[test]
    fn test_action_type_matching() {
        let t1 = ActionType::Transfer {
            asset: "FAT".to_string(),
        };
        let t2 = ActionType::Transfer {
            asset: "FAT".to_string(),
        };
        let t3 = ActionType::Transfer {
            asset: "ETH".to_string(),
        };

        assert!(t1.matches(&t2));
        assert!(!t1.matches(&t3));
        assert!(ActionType::Any.matches(&t1));
    }
}
