//! Message Router
//!
//! Routes messages between channels and the agent runtime.

use super::{AgentResponse, ChannelAdapter, ChannelError, MessageChannel, UserMessage};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};

/// Message router
pub struct MessageRouter {
    /// Registered channel IDs
    channel_ids: RwLock<Vec<String>>,

    /// Incoming message sender
    incoming_tx: mpsc::Sender<UserMessage>,

    /// Incoming message receiver (for runtime)
    incoming_rx: RwLock<Option<mpsc::Receiver<UserMessage>>>,
}

impl MessageRouter {
    /// Create new message router
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(1000);

        Self {
            channel_ids: RwLock::new(Vec::new()),
            incoming_tx: tx,
            incoming_rx: RwLock::new(Some(rx)),
        }
    }

    /// Register a channel
    pub async fn register_channel(
        &self,
        channel: MessageChannel,
    ) -> Result<(), ChannelError> {
        let channel_id = channel.id();

        // Check not already registered
        let mut ids = self.channel_ids.write().await;
        if ids.contains(&channel_id) {
            return Err(ChannelError::AlreadyConnected(channel_id));
        }

        ids.push(channel_id);
        Ok(())
    }

    /// Subscribe to incoming messages
    pub async fn subscribe(&self) -> Result<mpsc::Receiver<UserMessage>, ChannelError> {
        self.incoming_rx
            .write()
            .await
            .take()
            .ok_or(ChannelError::NotFound("already subscribed".to_string()))
    }

    /// Send response to channel
    pub async fn send_response(&self, response: AgentResponse) -> Result<(), ChannelError> {
        // In production: Route to appropriate channel adapter
        tracing::info!("Sending response to channel: {}", response.channel);
        Ok(())
    }

    /// Get list of connected channels
    pub fn connected_channels(&self) -> Vec<String> {
        // Simplified - return empty for sync context
        Vec::new()
    }

    /// Get list of connected channels (async)
    pub async fn connected_channels_async(&self) -> Vec<String> {
        self.channel_ids.read().await.clone()
    }

    /// Disconnect a channel
    pub async fn disconnect_channel(&self, channel_id: &str) -> Result<(), ChannelError> {
        let mut ids = self.channel_ids.write().await;
        if let Some(pos) = ids.iter().position(|id| id == channel_id) {
            ids.remove(pos);
            Ok(())
        } else {
            Err(ChannelError::NotFound(channel_id.to_string()))
        }
    }
}

impl Default for MessageRouter {
    fn default() -> Self {
        Self::new()
    }
}

// === Channel Adapters ===

/// Telegram adapter
pub struct TelegramAdapter {
    channel: MessageChannel,
    connected: bool,
}

impl TelegramAdapter {
    pub fn new(channel: MessageChannel) -> Self {
        Self {
            channel,
            connected: false,
        }
    }
}

#[async_trait::async_trait]
impl ChannelAdapter for TelegramAdapter {
    async fn connect(&mut self) -> Result<(), ChannelError> {
        self.connected = true;
        Ok(())
    }

    async fn disconnect(&mut self) -> Result<(), ChannelError> {
        self.connected = false;
        Ok(())
    }

    fn is_connected(&self) -> bool {
        self.connected
    }

    async fn receive(&mut self) -> Result<UserMessage, ChannelError> {
        tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
        Err(ChannelError::NotFound("no messages".to_string()))
    }

    async fn send(&self, response: AgentResponse) -> Result<(), ChannelError> {
        tracing::info!("Telegram: Sending response to {}", response.channel);
        Ok(())
    }

    fn channel_info(&self) -> &MessageChannel {
        &self.channel
    }
}
