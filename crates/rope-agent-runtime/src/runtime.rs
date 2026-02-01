//! RopeAgent Runtime
//!
//! Main runtime that orchestrates all components.

use crate::agents::{PersonalAgent, PersonalCapability};
use crate::channels::{AgentResponse, MessageChannel, MessageRouter, UserMessage};
use crate::config::RuntimeConfig;
use crate::error::RuntimeError;
use crate::identity::DatawalletIdentity;
use crate::lattice_client::{LatticeClient, LatticeEvent, TestimonyStatus};
use crate::memory::EncryptedMemoryStore;
use crate::skills::SkillRegistry;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;

/// RopeAgent Local Runtime
pub struct RopeAgentRuntime {
    /// User's Datawallet+ identity
    datawallet: DatawalletIdentity,

    /// Personal AI agent
    agent: Arc<RwLock<PersonalAgent>>,

    /// Message router
    message_router: Arc<MessageRouter>,

    /// Lattice client
    lattice_client: Arc<RwLock<LatticeClient>>,

    /// Encrypted memory store
    memory: Arc<EncryptedMemoryStore>,

    /// Skill registry
    skills: Arc<RwLock<SkillRegistry>>,

    /// Runtime configuration
    config: RuntimeConfig,

    /// Shutdown signal
    shutdown: Arc<RwLock<bool>>,
}

impl RopeAgentRuntime {
    /// Initialize runtime with Datawallet+ identity
    pub async fn initialize(
        datawallet: DatawalletIdentity,
        config: RuntimeConfig,
    ) -> Result<Self, RuntimeError> {
        // Initialize memory store
        let memory = EncryptedMemoryStore::open(&config.memory_path, datawallet.seed())?;

        // Initialize lattice client
        let lattice_client = LatticeClient::new(config.lattice_endpoints.clone());

        // Initialize personal agent
        let agent = PersonalAgent::new(datawallet.clone(), config.enabled_capabilities.clone());

        // Initialize skill registry
        let skills = SkillRegistry::new();

        Ok(Self {
            datawallet,
            agent: Arc::new(RwLock::new(agent)),
            message_router: Arc::new(MessageRouter::new()),
            lattice_client: Arc::new(RwLock::new(lattice_client)),
            memory: Arc::new(memory),
            skills: Arc::new(RwLock::new(skills)),
            config,
            shutdown: Arc::new(RwLock::new(false)),
        })
    }

    /// Get agent identity
    pub fn identity(&self) -> &DatawalletIdentity {
        &self.datawallet
    }

    /// Get configuration
    pub fn config(&self) -> &RuntimeConfig {
        &self.config
    }

    /// Connect to lattice network
    pub async fn connect_lattice(&self) -> Result<(), RuntimeError> {
        let mut client = self.lattice_client.write().await;
        client.connect().await?;

        tracing::info!("Connected to Datachain Rope lattice");
        Ok(())
    }

    /// Connect a messaging channel
    pub async fn connect_channel(&self, channel: MessageChannel) -> Result<(), RuntimeError> {
        // Register with message router
        self.message_router
            .register_channel(channel.clone())
            .await?;

        // Register with agent
        self.agent.write().await.connect_channel(channel.id());

        // Store in memory
        if let Some(creds) = channel.credentials() {
            self.memory
                .store_credentials(&channel.id(), &creds.ciphertext)?;
        }

        tracing::info!("Connected channel: {}", channel.id());
        Ok(())
    }

    /// Disconnect a channel
    pub async fn disconnect_channel(&self, channel_id: &str) -> Result<(), RuntimeError> {
        self.message_router.disconnect_channel(channel_id).await?;
        self.agent.write().await.disconnect_channel(channel_id);

        tracing::info!("Disconnected channel: {}", channel_id);
        Ok(())
    }

    /// Load a skill
    pub async fn load_skill(&self, skill: crate::skills::Skill) -> Result<(), RuntimeError> {
        // Verify governance approval
        skill.verify_governance_approval()?;

        let skill_name = skill.name.clone();

        // Register with skill registry
        self.skills.write().await.register(skill.clone());

        // Load into agent
        self.agent.write().await.load_skill(skill)?;

        tracing::info!("Loaded skill: {}", skill_name);
        Ok(())
    }

    /// Start runtime event loop
    pub async fn run(&self) -> Result<(), RuntimeError> {
        tracing::info!("Starting RopeAgent runtime...");

        // Connect to lattice
        self.connect_lattice().await?;

        // Subscribe to events
        let mut lattice_events = self.lattice_client.write().await.subscribe_events();
        let mut message_events = self.message_router.subscribe().await?;

        // Event loop
        loop {
            // Check shutdown
            if *self.shutdown.read().await {
                tracing::info!("Shutdown requested, stopping runtime");
                break;
            }

            tokio::select! {
                // Handle incoming messages
                Some(message) = message_events.recv() => {
                    if let Err(e) = self.handle_message(message).await {
                        tracing::error!("Error handling message: {:?}", e);
                    }
                }

                // Handle lattice events
                Some(event) = lattice_events.recv() => {
                    if let Err(e) = self.handle_lattice_event(event).await {
                        tracing::error!("Error handling lattice event: {:?}", e);
                    }
                }

                // Periodic tasks
                _ = tokio::time::sleep(Duration::from_secs(60)) => {
                    self.periodic_tasks().await;
                }
            }
        }

        // Cleanup
        self.shutdown().await?;

        Ok(())
    }

    /// Handle incoming message
    async fn handle_message(&self, message: UserMessage) -> Result<(), RuntimeError> {
        tracing::debug!(
            "Received message from {}: {:?}",
            message.sender,
            message.content
        );

        // Log to memory
        self.memory
            .log_event(crate::memory::Event::MessageReceived {
                channel: message.channel.clone(),
                timestamp: message.timestamp,
            })?;

        // Process through agent
        let response = self.agent.write().await.process_message(message).await?;

        // Send response
        self.message_router.send_response(response).await?;

        Ok(())
    }

    /// Handle lattice event
    async fn handle_lattice_event(&self, event: LatticeEvent) -> Result<(), RuntimeError> {
        match event {
            LatticeEvent::TestimonyResult {
                action_id,
                status,
                testimonies,
            } => {
                tracing::info!(
                    "Testimony result for {}: {:?}",
                    hex::encode(&action_id[..8]),
                    status
                );

                let approved = matches!(status, TestimonyStatus::Approved { .. });

                // Update agent
                self.agent.write().await.handle_testimony_result(
                    action_id,
                    approved,
                    testimonies,
                )?;

                // Log event
                self.memory
                    .log_event(crate::memory::Event::TestimonyReceived {
                        action_id,
                        approved,
                    })?;

                // If approved, execute action
                if let TestimonyStatus::Approved { authorization } = status {
                    self.execute_authorized_action(action_id, authorization)
                        .await?;
                }
            }

            LatticeEvent::SkillUpdate { skill_id, version } => {
                tracing::info!(
                    "Skill update available: {} v{}",
                    hex::encode(&skill_id[..8]),
                    version
                );
                // In production: Auto-update if configured
            }

            LatticeEvent::SecurityAlert {
                alert_type,
                details,
            } => {
                tracing::warn!("Security alert: {} - {}", alert_type, details);
                self.memory.log_event(crate::memory::Event::SecurityAlert {
                    alert_type,
                    details,
                })?;
            }

            LatticeEvent::OesEpochChanged { new_epoch } => {
                tracing::info!("OES epoch changed to {}", new_epoch);
                // Update identity epoch
            }

            LatticeEvent::NetworkStatus { connected } => {
                tracing::info!(
                    "Network status: {}",
                    if connected {
                        "connected"
                    } else {
                        "disconnected"
                    }
                );
            }
        }

        Ok(())
    }

    /// Execute authorized action
    async fn execute_authorized_action(
        &self,
        action_id: [u8; 32],
        authorization: crate::lattice_client::ExecutionAuthorization,
    ) -> Result<(), RuntimeError> {
        tracing::info!(
            "Executing authorized action {}",
            hex::encode(&action_id[..8])
        );

        // Get pending action from agent
        let pending = self.agent.read().await.get_pending_action(&action_id);

        if let Some(action) = pending {
            // In production: Execute through VettedToolRegistry
            tracing::info!("Action type: {:?}", action.intent.intent_type);

            // Record execution
            let record = crate::lattice_client::ExecutionRecord {
                action_id,
                authorization_ref: authorization.action_id,
                success: true,
                tx_hash: Some([0u8; 32]), // Placeholder
                proof: None,
                fee_used: Some(21000),
                timestamp: chrono::Utc::now().timestamp(),
            };

            self.lattice_client
                .read()
                .await
                .record_execution(record)
                .await?;

            // Notify user
            let response = AgentResponse {
                channel: action.message.channel.clone(),
                content: crate::channels::ResponseContent::Text(format!(
                    "✅ Action completed successfully!\n\n\
                         Action ID: {}\n\
                         Approved by: {} agents",
                    hex::encode(&action_id[..8]),
                    authorization.authorized_by.len()
                )),
                reply_to: action.message.message_id.clone(),
            };

            self.message_router.send_response(response).await?;
        }

        Ok(())
    }

    /// Periodic tasks
    async fn periodic_tasks(&self) {
        // Cleanup expired pending actions
        self.agent.write().await.cleanup_pending_actions(3600);

        // Flush memory
        if let Err(e) = self.memory.flush() {
            tracing::error!("Failed to flush memory: {:?}", e);
        }

        // Health check
        tracing::debug!("Runtime healthy");
    }

    /// Request shutdown
    pub async fn request_shutdown(&self) {
        *self.shutdown.write().await = true;
    }

    /// Graceful shutdown
    async fn shutdown(&self) -> Result<(), RuntimeError> {
        tracing::info!("Shutting down RopeAgent runtime...");

        // Disconnect from lattice
        self.lattice_client.write().await.disconnect().await;

        // Flush memory
        self.memory.flush()?;

        tracing::info!("Runtime shutdown complete");
        Ok(())
    }

    /// Get connected channels
    pub fn connected_channels(&self) -> Vec<String> {
        self.message_router.connected_channels()
    }

    /// Get connected channels (async version)
    pub async fn connected_channels_async(&self) -> Vec<String> {
        self.message_router.connected_channels_async().await
    }

    /// Check if lattice is connected
    pub async fn is_lattice_connected(&self) -> bool {
        self.lattice_client.read().await.is_connected()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_identity() -> DatawalletIdentity {
        DatawalletIdentity::new([1u8; 32], vec![0u8; 64], "Test User".to_string())
    }

    #[tokio::test]
    async fn test_runtime_initialization() {
        let config = RuntimeConfig::development();
        let identity = test_identity();

        let runtime = RopeAgentRuntime::initialize(identity, config).await;
        assert!(runtime.is_ok());
    }

    #[tokio::test]
    async fn test_connect_channel() {
        let config = RuntimeConfig::development();
        let identity = test_identity();

        let runtime = RopeAgentRuntime::initialize(identity, config)
            .await
            .unwrap();

        let channel = MessageChannel::Telegram {
            bot_token: crate::channels::EncryptedCredentials::placeholder(),
            allowed_users: vec![],
        };

        let result = runtime.connect_channel(channel).await;
        assert!(result.is_ok());
        assert_eq!(runtime.connected_channels_async().await.len(), 1);
    }
}
