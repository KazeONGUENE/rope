//! Message Channels
//!
//! Adapters for various messaging platforms (WhatsApp, Telegram, Slack, Discord, etc.)

mod router;

pub use router::*;

use crate::error::ChannelError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Message channel types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessageChannel {
    /// WhatsApp
    WhatsApp {
        phone: String,
        credentials: EncryptedCredentials,
    },

    /// Telegram
    Telegram {
        bot_token: EncryptedCredentials,
        allowed_users: Vec<i64>,
    },

    /// Discord
    Discord {
        bot_token: EncryptedCredentials,
        guild_ids: Vec<u64>,
    },

    /// Slack
    Slack {
        workspace: String,
        bot_token: EncryptedCredentials,
    },

    /// iMessage (macOS only)
    IMessage { apple_id: String },

    /// Email
    Email {
        address: String,
        imap_credentials: EncryptedCredentials,
        smtp_credentials: EncryptedCredentials,
    },

    /// Custom channel
    Custom {
        name: String,
        adapter: String,
        config: HashMap<String, String>,
    },
}

impl MessageChannel {
    /// Get channel ID
    pub fn id(&self) -> String {
        match self {
            MessageChannel::WhatsApp { phone, .. } => format!("whatsapp:{}", phone),
            MessageChannel::Telegram { .. } => "telegram".to_string(),
            MessageChannel::Discord { .. } => "discord".to_string(),
            MessageChannel::Slack { workspace, .. } => format!("slack:{}", workspace),
            MessageChannel::IMessage { apple_id } => format!("imessage:{}", apple_id),
            MessageChannel::Email { address, .. } => format!("email:{}", address),
            MessageChannel::Custom { name, .. } => format!("custom:{}", name),
        }
    }

    /// Get channel type name
    pub fn type_name(&self) -> &'static str {
        match self {
            MessageChannel::WhatsApp { .. } => "whatsapp",
            MessageChannel::Telegram { .. } => "telegram",
            MessageChannel::Discord { .. } => "discord",
            MessageChannel::Slack { .. } => "slack",
            MessageChannel::IMessage { .. } => "imessage",
            MessageChannel::Email { .. } => "email",
            MessageChannel::Custom { .. } => "custom",
        }
    }

    /// Extract credentials for storage
    pub fn credentials(&self) -> Option<&EncryptedCredentials> {
        match self {
            MessageChannel::WhatsApp { credentials, .. } => Some(credentials),
            MessageChannel::Telegram { bot_token, .. } => Some(bot_token),
            MessageChannel::Discord { bot_token, .. } => Some(bot_token),
            MessageChannel::Slack { bot_token, .. } => Some(bot_token),
            MessageChannel::Email {
                imap_credentials, ..
            } => Some(imap_credentials),
            _ => None,
        }
    }

    /// Verify credentials are valid
    pub fn verify_credentials(&self) -> Result<(), ChannelError> {
        // In production, actually verify with the platform
        Ok(())
    }
}

/// Encrypted credentials
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedCredentials {
    /// Encrypted data
    pub ciphertext: Vec<u8>,

    /// Nonce used for encryption
    pub nonce: [u8; 16],

    /// OES epoch when encrypted
    pub oes_epoch: u64,
}

impl EncryptedCredentials {
    /// Create new encrypted credentials
    pub fn new(ciphertext: Vec<u8>) -> Self {
        Self {
            ciphertext,
            nonce: [0u8; 16],
            oes_epoch: 0,
        }
    }

    /// Create placeholder (for testing)
    pub fn placeholder() -> Self {
        Self::new(vec![])
    }
}

/// User message from any channel
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserMessage {
    /// Source channel
    pub channel: String,

    /// Sender identifier
    pub sender: String,

    /// Message content
    pub content: MessageContent,

    /// Timestamp
    pub timestamp: i64,

    /// Platform-specific message ID
    pub message_id: Option<String>,

    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Message content types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MessageContent {
    /// Plain text message
    Text(String),

    /// Image with optional caption
    Image {
        url: String,
        caption: Option<String>,
    },

    /// Audio message (voice note)
    Audio {
        url: String,
        transcription: Option<String>,
    },

    /// Document/file
    Document { url: String, name: String },

    /// Location
    Location { lat: f64, lng: f64 },

    /// Command (e.g., /start, /help)
    Command { name: String, args: Vec<String> },

    /// Reaction to another message
    Reaction {
        emoji: String,
        target_message_id: String,
    },
}

impl MessageContent {
    /// Get text content (if any)
    pub fn as_text(&self) -> Option<&str> {
        match self {
            MessageContent::Text(text) => Some(text),
            MessageContent::Image { caption, .. } => caption.as_deref(),
            _ => None,
        }
    }
}

/// Agent response to send back
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AgentResponse {
    /// Target channel
    pub channel: String,

    /// Response content
    pub content: ResponseContent,

    /// Reply to specific message
    pub reply_to: Option<String>,
}

/// Response content types
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ResponseContent {
    /// Text response
    Text(String),

    /// Text with markdown formatting
    Markdown(String),

    /// Rich embed (for Discord/Slack)
    Embed {
        title: String,
        description: String,
        fields: Vec<(String, String)>,
        color: Option<u32>,
    },

    /// Buttons/quick replies
    Buttons {
        text: String,
        buttons: Vec<(String, String)>,
    },

    /// Error response
    Error(String),
}

/// Channel adapter trait
#[async_trait]
pub trait ChannelAdapter: Send + Sync {
    /// Connect to the channel
    async fn connect(&mut self) -> Result<(), ChannelError>;

    /// Disconnect from the channel
    async fn disconnect(&mut self) -> Result<(), ChannelError>;

    /// Check if connected
    fn is_connected(&self) -> bool;

    /// Receive next message
    async fn receive(&mut self) -> Result<UserMessage, ChannelError>;

    /// Send response
    async fn send(&self, response: AgentResponse) -> Result<(), ChannelError>;

    /// Get channel info
    fn channel_info(&self) -> &MessageChannel;
}
