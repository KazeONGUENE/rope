//! WebSocket Support for Real-Time Lattice Events
//!
//! Provides client and server for WebSocket communication

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

/// Lattice event types
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LatticeEvent {
    /// New string added to lattice
    StringCreated {
        id: String,
        creator: String,
        string_type: String,
        timestamp: i64,
    },

    /// Testimony received
    TestimonyReceived {
        action_id: String,
        agent_id: String,
        verdict: String,
        confidence: f64,
    },

    /// Consensus reached
    ConsensusReached {
        action_id: String,
        approved: bool,
        testimonies: u32,
    },

    /// OES epoch changed
    OesEpochChanged {
        epoch: u64,
        state_hash: String,
    },

    /// Skill updated
    SkillUpdated {
        skill_id: String,
        version: String,
    },

    /// Security alert
    SecurityAlert {
        alert_type: String,
        severity: String,
        details: String,
    },

    /// Connection status
    ConnectionStatus {
        connected: bool,
    },

    /// Heartbeat
    Ping,
    Pong,
}

/// WebSocket command from client
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WebSocketCommand {
    /// Subscribe to events
    Subscribe { event_types: Vec<String> },

    /// Unsubscribe from events
    Unsubscribe { event_types: Vec<String> },

    /// Ping for keepalive
    Ping,

    /// Authenticate
    Authenticate { token: String },
}

/// WebSocket client for connecting to Lattice
pub struct LatticeWebSocketClient {
    url: String,
    event_tx: broadcast::Sender<LatticeEvent>,
    command_tx: Option<mpsc::Sender<WebSocketCommand>>,
    connected: Arc<std::sync::atomic::AtomicBool>,
}

impl LatticeWebSocketClient {
    /// Create new client
    pub fn new(url: &str) -> Self {
        let (event_tx, _) = broadcast::channel(1000);

        Self {
            url: url.to_string(),
            event_tx,
            command_tx: None,
            connected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Subscribe to events
    pub fn subscribe(&self) -> broadcast::Receiver<LatticeEvent> {
        self.event_tx.subscribe()
    }

    /// Check if connected
    pub fn is_connected(&self) -> bool {
        self.connected.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Connect to WebSocket server
    pub async fn connect(&mut self) -> Result<(), WebSocketError> {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{connect_async, tungstenite::Message};

        let (ws_stream, _) = connect_async(&self.url)
            .await
            .map_err(|e| WebSocketError::ConnectionFailed(e.to_string()))?;

        let (mut write, mut read) = ws_stream.split();

        self.connected
            .store(true, std::sync::atomic::Ordering::SeqCst);

        // Broadcast connection event
        let _ = self
            .event_tx
            .send(LatticeEvent::ConnectionStatus { connected: true });

        // Create command channel
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<WebSocketCommand>(100);
        self.command_tx = Some(cmd_tx);

        let event_tx = self.event_tx.clone();
        let connected = self.connected.clone();

        // Spawn read task
        let read_task = tokio::spawn(async move {
            while let Some(msg) = read.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(event) = serde_json::from_str::<LatticeEvent>(&text) {
                            let _ = event_tx.send(event);
                        }
                    }
                    Ok(Message::Close(_)) => {
                        connected.store(false, std::sync::atomic::Ordering::SeqCst);
                        let _ = event_tx.send(LatticeEvent::ConnectionStatus { connected: false });
                        break;
                    }
                    Ok(Message::Ping(_)) => {
                        // Handled by tungstenite automatically
                    }
                    Err(_) => {
                        connected.store(false, std::sync::atomic::Ordering::SeqCst);
                        let _ = event_tx.send(LatticeEvent::ConnectionStatus { connected: false });
                        break;
                    }
                    _ => {}
                }
            }
        });

        // Spawn write task
        let write_task = tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                if let Ok(json) = serde_json::to_string(&cmd) {
                    if write.send(Message::Text(json)).await.is_err() {
                        break;
                    }
                }
            }
        });

        // Spawn heartbeat task
        let cmd_tx_heartbeat = self.command_tx.clone();
        let connected_heartbeat = self.connected.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

            loop {
                interval.tick().await;

                if !connected_heartbeat.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }

                if let Some(tx) = &cmd_tx_heartbeat {
                    let _ = tx.send(WebSocketCommand::Ping).await;
                }
            }
        });

        Ok(())
    }

    /// Send command
    pub async fn send_command(&self, command: WebSocketCommand) -> Result<(), WebSocketError> {
        if let Some(tx) = &self.command_tx {
            tx.send(command)
                .await
                .map_err(|e| WebSocketError::SendFailed(e.to_string()))?;
            Ok(())
        } else {
            Err(WebSocketError::NotConnected)
        }
    }

    /// Subscribe to specific event types
    pub async fn subscribe_events(&self, event_types: Vec<String>) -> Result<(), WebSocketError> {
        self.send_command(WebSocketCommand::Subscribe { event_types })
            .await
    }

    /// Disconnect
    pub fn disconnect(&self) {
        self.connected
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// WebSocket errors
#[derive(Debug, thiserror::Error)]
pub enum WebSocketError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Not connected")]
    NotConnected,

    #[error("Send failed: {0}")]
    SendFailed(String),

    #[error("Authentication failed")]
    AuthenticationFailed,
}

/// Event filter for subscriptions
pub struct EventFilter {
    subscribed_types: std::collections::HashSet<String>,
}

impl EventFilter {
    pub fn new() -> Self {
        Self {
            subscribed_types: std::collections::HashSet::new(),
        }
    }

    pub fn subscribe(&mut self, event_type: &str) {
        self.subscribed_types.insert(event_type.to_string());
    }

    pub fn unsubscribe(&mut self, event_type: &str) {
        self.subscribed_types.remove(event_type);
    }

    pub fn should_emit(&self, event: &LatticeEvent) -> bool {
        if self.subscribed_types.is_empty() || self.subscribed_types.contains("*") {
            return true;
        }

        let event_type = match event {
            LatticeEvent::StringCreated { .. } => "StringCreated",
            LatticeEvent::TestimonyReceived { .. } => "TestimonyReceived",
            LatticeEvent::ConsensusReached { .. } => "ConsensusReached",
            LatticeEvent::OesEpochChanged { .. } => "OesEpochChanged",
            LatticeEvent::SkillUpdated { .. } => "SkillUpdated",
            LatticeEvent::SecurityAlert { .. } => "SecurityAlert",
            LatticeEvent::ConnectionStatus { .. } => "ConnectionStatus",
            LatticeEvent::Ping | LatticeEvent::Pong => "Heartbeat",
        };

        self.subscribed_types.contains(event_type)
    }
}

impl Default for EventFilter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_event_filter() {
        let mut filter = EventFilter::new();

        // Empty filter accepts all
        assert!(filter.should_emit(&LatticeEvent::Ping));

        // Subscribe to specific type
        filter.subscribe("TestimonyReceived");

        assert!(filter.should_emit(&LatticeEvent::TestimonyReceived {
            action_id: "test".to_string(),
            agent_id: "agent".to_string(),
            verdict: "approve".to_string(),
            confidence: 0.9,
        }));

        assert!(!filter.should_emit(&LatticeEvent::Ping));
    }

    #[test]
    fn test_event_serialization() {
        let event = LatticeEvent::ConsensusReached {
            action_id: "abc123".to_string(),
            approved: true,
            testimonies: 5,
        };

        let json = serde_json::to_string(&event).unwrap();
        assert!(json.contains("ConsensusReached"));
        assert!(json.contains("abc123"));

        let parsed: LatticeEvent = serde_json::from_str(&json).unwrap();
        match parsed {
            LatticeEvent::ConsensusReached {
                approved,
                testimonies,
                ..
            } => {
                assert!(approved);
                assert_eq!(testimonies, 5);
            }
            _ => panic!("Wrong event type"),
        }
    }
}
