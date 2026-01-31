//! Personal Agent
//!
//! OpenClaw-style autonomous AI assistant with blockchain verification.

use super::{AgentStatus, DailyLimits, PersonalCapability, UsageTracker};
use crate::channels::{AgentResponse, MessageContent, ResponseContent, UserMessage};
use crate::error::RuntimeError;
use crate::identity::{AuthorizationToken, DatawalletIdentity, RopeAgentIdentity};
use crate::intent::{ActionType, Intent, IntentParser};
use crate::skills::{Skill, SkillRegistry};
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

/// Personal AI Agent - OpenClaw-style autonomous assistant
pub struct PersonalAgent {
    /// Agent identity (bound to Datawallet+)
    identity: RopeAgentIdentity,

    /// Enabled capabilities
    capabilities: Vec<PersonalCapability>,

    /// Loaded skills
    skills: RwLock<SkillRegistry>,

    /// Connected channels
    channels: RwLock<Vec<String>>,

    /// Daily limits
    limits: DailyLimits,

    /// Usage tracker
    usage: RwLock<UsageTracker>,

    /// Current status
    status: RwLock<AgentStatus>,

    /// Pending actions awaiting testimony
    pending_actions: RwLock<HashMap<[u8; 32], PendingAction>>,

    /// Intent parser
    intent_parser: IntentParser,

    /// Conversation state per channel
    conversation_state: RwLock<HashMap<String, ConversationState>>,
}

impl PersonalAgent {
    /// Create new personal agent
    pub fn new(datawallet: DatawalletIdentity, capabilities: Vec<PersonalCapability>) -> Self {
        let identity = RopeAgentIdentity::new(datawallet);

        Self {
            identity,
            capabilities,
            skills: RwLock::new(SkillRegistry::new()),
            channels: RwLock::new(Vec::new()),
            limits: DailyLimits::default(),
            usage: RwLock::new(UsageTracker::default()),
            status: RwLock::new(AgentStatus::Idle),
            pending_actions: RwLock::new(HashMap::new()),
            intent_parser: IntentParser::new(),
            conversation_state: RwLock::new(HashMap::new()),
        }
    }

    /// Get agent identity
    pub fn identity(&self) -> &RopeAgentIdentity {
        &self.identity
    }

    /// Get current status
    pub fn status(&self) -> AgentStatus {
        self.status.read().clone()
    }

    /// Set status
    fn set_status(&self, status: AgentStatus) {
        *self.status.write() = status;
    }

    /// Check if capability is enabled
    pub fn has_capability(&self, capability: &PersonalCapability) -> bool {
        self.capabilities.contains(capability)
    }

    /// Connect a channel
    pub fn connect_channel(&self, channel_id: String) {
        let mut channels = self.channels.write();
        if !channels.contains(&channel_id) {
            channels.push(channel_id);
        }
    }

    /// Disconnect a channel
    pub fn disconnect_channel(&self, channel_id: &str) {
        self.channels.write().retain(|c| c != channel_id);
    }

    /// Load a skill
    pub fn load_skill(&self, skill: Skill) -> Result<(), RuntimeError> {
        // Check required capabilities
        for required in &skill.required_capabilities {
            if !self.capabilities.iter().any(|c| c == required) {
                return Err(RuntimeError::SkillError(
                    crate::error::SkillError::MissingCapability(format!("{:?}", required)),
                ));
            }
        }

        self.skills.write().register(skill);
        Ok(())
    }

    /// Process incoming message
    pub async fn process_message(
        &self,
        message: UserMessage,
    ) -> Result<AgentResponse, RuntimeError> {
        // Update status
        self.set_status(AgentStatus::Processing);

        // Check daily limits
        self.check_limits()?;

        // Update conversation state
        self.update_conversation_state(&message);

        // Extract text content
        let text = match &message.content {
            MessageContent::Text(t) => t.clone(),
            MessageContent::Command { name, args } => {
                format!("/{} {}", name, args.join(" "))
            }
            _ => {
                // Non-text messages
                return Ok(self.create_response(
                    &message.channel,
                    ResponseContent::Text(
                        "I can currently only process text messages.".to_string(),
                    ),
                ));
            }
        };

        // Parse intent
        let intent = self.intent_parser.parse(&text);

        // Check if action requires testimony
        if intent.requires_testimony() {
            return self.process_with_testimony(message, intent).await;
        }

        // Process locally
        let response = self.process_local(message.clone(), intent).await?;

        // Update usage
        self.usage.write().record_message();

        // Reset status
        self.set_status(AgentStatus::Idle);

        Ok(response)
    }

    /// Process action that requires testimony consensus
    async fn process_with_testimony(
        &self,
        message: UserMessage,
        intent: Intent,
    ) -> Result<AgentResponse, RuntimeError> {
        // Generate action ID
        let action_id = self.generate_action_id(&intent);

        // Create authorization token
        let value = intent.estimated_value_usd();
        let _token = self.identity.clone().create_authorization_token(
            intent.to_action_type(),
            value,
            std::time::Duration::from_secs(intent.timeout_secs()),
        );

        // Create pending action
        let pending = PendingAction {
            id: action_id,
            intent: intent.clone(),
            message: message.clone(),
            created_at: chrono::Utc::now().timestamp(),
            status: PendingActionStatus::AwaitingTestimony,
            testimonies: Vec::new(),
        };

        self.pending_actions.write().insert(action_id, pending);

        // Update status
        self.set_status(AgentStatus::AwaitingTestimony { action_id });

        // Return acknowledgment
        let response_text = match &intent.intent_type {
            crate::intent::IntentType::Transfer {
                asset,
                amount,
                recipient,
            } => {
                format!(
                    "Processing transfer of {} {} to {}...\n\n\
                     Awaiting AI Testimony consensus for verification.",
                    amount, asset, recipient
                )
            }
            crate::intent::IntentType::Swap {
                from_asset,
                to_asset,
                amount,
            } => {
                format!(
                    "Processing swap of {} {} to {}...\n\n\
                     Awaiting AI Testimony consensus for verification.",
                    amount, from_asset, to_asset
                )
            }
            _ => "Processing action... Awaiting AI Testimony consensus.".to_string(),
        };

        Ok(self.create_response(&message.channel, ResponseContent::Text(response_text)))
    }

    /// Process action locally (no testimony needed)
    async fn process_local(
        &self,
        message: UserMessage,
        intent: Intent,
    ) -> Result<AgentResponse, RuntimeError> {
        let response_text = match &intent.intent_type {
            crate::intent::IntentType::Help => self.generate_help_text(),
            crate::intent::IntentType::Status { resource } => self.generate_status_text(resource),
            crate::intent::IntentType::Query { topic } => {
                format!("You asked about: {}\n\nI'm processing your query...", topic)
            }
            crate::intent::IntentType::SetReminder { time, message: msg } => {
                let dt = chrono::DateTime::from_timestamp(*time, 0)
                    .map(|d| d.format("%Y-%m-%d %H:%M UTC").to_string())
                    .unwrap_or_else(|| "unknown time".to_string());
                format!("Reminder set for {}:\n\"{}\"", dt, msg)
            }
            _ => "I'm not sure how to help with that. Try asking for /help".to_string(),
        };

        Ok(self.create_response(&message.channel, ResponseContent::Text(response_text)))
    }

    /// Generate help text
    fn generate_help_text(&self) -> String {
        let mut help = String::from("🤖 **RopeAgent Help**\n\n");
        help.push_str("I'm your personal AI assistant secured by Datachain Rope.\n\n");
        help.push_str("**Available Commands:**\n");
        help.push_str("• `transfer [amount] [asset] to [address]` - Transfer tokens\n");
        help.push_str("• `swap [amount] [from] to [to]` - Swap tokens\n");
        help.push_str("• `stake [amount]` - Stake FAT tokens\n");
        help.push_str("• `status` - Check your balance\n");
        help.push_str("• `remind [time] [message]` - Set a reminder\n");
        help.push_str("• `help` - Show this help\n\n");
        help.push_str("**Enabled Capabilities:**\n");
        for cap in &self.capabilities {
            help.push_str(&format!("• {:?}\n", cap));
        }
        help
    }

    /// Generate status text
    fn generate_status_text(&self, resource: &str) -> String {
        match resource {
            "balance" => {
                format!(
                    "📊 **Account Status**\n\n\
                     Identity: {}\n\
                     Reputation: {}/100\n\
                     Messages Today: {}\n\
                     Skills Loaded: {}",
                    &self.identity.datawallet.did,
                    self.identity.reputation,
                    self.usage.read().messages_today,
                    self.skills.read().skill_count(),
                )
            }
            _ => format!("Status for '{}' not available.", resource),
        }
    }

    /// Check daily limits
    fn check_limits(&self) -> Result<(), RuntimeError> {
        let mut usage = self.usage.write();
        usage.check_reset(self.limits.reset_hour);

        if usage.messages_today >= self.limits.max_messages {
            return Err(RuntimeError::ExecutionError(
                "Daily message limit reached".to_string(),
            ));
        }

        Ok(())
    }

    /// Update conversation state
    fn update_conversation_state(&self, message: &UserMessage) {
        let mut state = self.conversation_state.write();
        let channel_state = state
            .entry(message.channel.clone())
            .or_insert_with(ConversationState::new);

        channel_state.add_message(message);
    }

    /// Generate action ID
    fn generate_action_id(&self, intent: &Intent) -> [u8; 32] {
        let mut data = Vec::new();
        data.extend_from_slice(&self.identity.datawallet.node_id);
        data.extend_from_slice(intent.raw_text.as_bytes());
        data.extend_from_slice(&chrono::Utc::now().timestamp().to_le_bytes());

        *blake3::hash(&data).as_bytes()
    }

    /// Create response
    fn create_response(&self, channel: &str, content: ResponseContent) -> AgentResponse {
        AgentResponse {
            channel: channel.to_string(),
            content,
            reply_to: None,
        }
    }

    /// Handle testimony result
    pub fn handle_testimony_result(
        &self,
        action_id: [u8; 32],
        approved: bool,
        testimonies: Vec<TestimonyResult>,
    ) -> Result<(), RuntimeError> {
        let mut pending = self.pending_actions.write();

        if let Some(action) = pending.get_mut(&action_id) {
            action.testimonies = testimonies;

            if approved {
                action.status = PendingActionStatus::Approved;
            } else {
                action.status = PendingActionStatus::Rejected;
            }
        }

        self.set_status(AgentStatus::Idle);
        Ok(())
    }

    /// Get pending action
    pub fn get_pending_action(&self, action_id: &[u8; 32]) -> Option<PendingAction> {
        self.pending_actions.read().get(action_id).cloned()
    }

    /// Clear completed pending actions
    pub fn cleanup_pending_actions(&self, max_age_secs: i64) {
        let now = chrono::Utc::now().timestamp();
        self.pending_actions
            .write()
            .retain(|_, action| now - action.created_at < max_age_secs);
    }
}

/// Pending action awaiting testimony
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingAction {
    /// Action ID
    pub id: [u8; 32],

    /// Parsed intent
    pub intent: Intent,

    /// Original message
    pub message: UserMessage,

    /// Creation timestamp
    pub created_at: i64,

    /// Current status
    pub status: PendingActionStatus,

    /// Received testimonies
    pub testimonies: Vec<TestimonyResult>,
}

/// Pending action status
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingActionStatus {
    /// Awaiting testimony consensus
    AwaitingTestimony,

    /// Approved by consensus
    Approved,

    /// Rejected by consensus
    Rejected,

    /// Executing
    Executing,

    /// Completed
    Completed,

    /// Failed
    Failed { error: String },

    /// Timed out
    TimedOut,
}

/// Testimony result from AI agent
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TestimonyResult {
    /// Agent ID
    pub agent_id: [u8; 32],

    /// Agent type
    pub agent_type: String,

    /// Verdict (approve/reject/abstain)
    pub verdict: String,

    /// Confidence score
    pub confidence: f64,

    /// Reasoning (may be encrypted)
    pub reasoning: Option<String>,
}

/// Conversation state for a channel
#[derive(Clone, Debug, Default)]
pub struct ConversationState {
    /// Recent messages
    messages: Vec<ConversationEntry>,

    /// Context variables
    context: HashMap<String, String>,
}

impl ConversationState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_message(&mut self, message: &UserMessage) {
        self.messages.push(ConversationEntry {
            role: "user".to_string(),
            content: message.content.as_text().unwrap_or("").to_string(),
            timestamp: message.timestamp,
        });

        // Keep only recent messages
        if self.messages.len() > 50 {
            self.messages.remove(0);
        }
    }
}

/// Conversation entry
#[derive(Clone, Debug, Serialize, Deserialize)]
struct ConversationEntry {
    role: String,
    content: String,
    timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_agent() -> PersonalAgent {
        let identity = DatawalletIdentity::new([1u8; 32], vec![0u8; 64], "Test Agent".to_string());

        PersonalAgent::new(
            identity,
            vec![PersonalCapability::Messaging, PersonalCapability::Calendar],
        )
    }

    #[test]
    fn test_agent_creation() {
        let agent = test_agent();
        assert!(agent.has_capability(&PersonalCapability::Messaging));
        assert!(!agent.has_capability(&PersonalCapability::Shell));
    }

    #[test]
    fn test_channel_management() {
        let agent = test_agent();

        agent.connect_channel("telegram".to_string());
        assert_eq!(agent.channels.read().len(), 1);

        agent.disconnect_channel("telegram");
        assert_eq!(agent.channels.read().len(), 0);
    }

    #[tokio::test]
    async fn test_process_help() {
        let agent = test_agent();

        let message = UserMessage {
            channel: "test".to_string(),
            sender: "user1".to_string(),
            content: MessageContent::Text("help".to_string()),
            timestamp: chrono::Utc::now().timestamp(),
            message_id: None,
            metadata: HashMap::new(),
        };

        let response = agent.process_message(message).await.unwrap();

        if let ResponseContent::Text(text) = response.content {
            assert!(text.contains("RopeAgent Help"));
        } else {
            panic!("Expected text response");
        }
    }
}
