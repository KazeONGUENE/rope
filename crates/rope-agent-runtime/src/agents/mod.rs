//! Personal Agent Implementation
//!
//! OpenClaw-style autonomous AI assistant with blockchain verification.

mod personal;

pub use personal::*;

use serde::{Deserialize, Serialize};

/// Personal agent capabilities
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PersonalCapability {
    /// Can send/receive messages on user's behalf
    Messaging,

    /// Can execute browser actions
    BrowserAutomation,

    /// Can manage calendar and scheduling
    Calendar,

    /// Can handle email
    Email,

    /// Can execute financial transactions (with limits)
    Financial { daily_limit: u64 },

    /// Can interact with smart contracts
    SmartContract,

    /// Can access file system (limited)
    FileSystem,

    /// Can execute shell commands (whitelisted)
    Shell,

    /// Custom capability
    Custom(String),
}

impl PersonalCapability {
    /// Check if capability is high-risk
    pub fn is_high_risk(&self) -> bool {
        matches!(
            self,
            PersonalCapability::Financial { .. }
                | PersonalCapability::SmartContract
                | PersonalCapability::Shell
                | PersonalCapability::FileSystem
        )
    }
}

/// Agent status
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentStatus {
    /// Agent is idle, waiting for messages
    Idle,

    /// Agent is processing a message
    Processing,

    /// Agent is waiting for testimony consensus
    AwaitingTestimony { action_id: [u8; 32] },

    /// Agent is executing an action
    Executing { action_id: [u8; 32] },

    /// Agent is paused
    Paused,

    /// Agent encountered an error
    Error { message: String },
}

/// Daily usage limits
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DailyLimits {
    /// Maximum messages per day
    pub max_messages: u32,

    /// Maximum financial value (USD)
    pub max_financial_value: u64,

    /// Maximum skill executions
    pub max_skill_executions: u32,

    /// Reset time (UTC hour)
    pub reset_hour: u8,
}

impl Default for DailyLimits {
    fn default() -> Self {
        Self {
            max_messages: 1000,
            max_financial_value: 10_000,
            max_skill_executions: 100,
            reset_hour: 0,
        }
    }
}

/// Usage tracking
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct UsageTracker {
    /// Messages sent today
    pub messages_today: u32,

    /// Financial value today (USD)
    pub financial_value_today: u64,

    /// Skill executions today
    pub skill_executions_today: u32,

    /// Last reset timestamp
    pub last_reset: i64,
}

impl UsageTracker {
    /// Check and reset if needed
    pub fn check_reset(&mut self, reset_hour: u8) {
        use chrono::Timelike;

        let now = chrono::Utc::now();
        let last_reset = chrono::DateTime::from_timestamp(self.last_reset, 0)
            .unwrap_or(chrono::DateTime::UNIX_EPOCH);

        // Check if we've passed the reset hour since last reset
        let should_reset = if now.date_naive() > last_reset.date_naive() {
            true
        } else if now.date_naive() == last_reset.date_naive() {
            now.hour() >= reset_hour as u32 && (last_reset.hour() as u8) < reset_hour
        } else {
            false
        };

        if should_reset {
            self.messages_today = 0;
            self.financial_value_today = 0;
            self.skill_executions_today = 0;
            self.last_reset = now.timestamp();
        }
    }

    /// Record message
    pub fn record_message(&mut self) {
        self.messages_today += 1;
    }

    /// Record financial transaction
    pub fn record_financial(&mut self, value_usd: u64) {
        self.financial_value_today += value_usd;
    }

    /// Record skill execution
    pub fn record_skill_execution(&mut self) {
        self.skill_executions_today += 1;
    }
}
