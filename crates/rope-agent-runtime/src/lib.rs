//! # RopeAgent Local Runtime
//!
//! OpenClaw-style blockchain-native AI agents for Datachain Rope.
//!
//! This crate provides:
//! - Secure local execution environment
//! - Message routing from chat platforms (WhatsApp, Telegram, Slack, Discord)
//! - AI-powered intent parsing and skill execution
//! - Encrypted memory persistence with OES
//! - String Lattice connectivity for Testimony consensus
//! - Security hardening (sandboxing, rate limiting, input validation)
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                    ROPEAGENT RUNTIME                         │
//! ├─────────────────────────────────────────────────────────────┤
//! │  Message Router → AI Model → Intent Parser → Testimony      │
//! │        ↓              ↓            ↓              ↓         │
//! │  Chat Channels   LLM (Local/     Skill       String         │
//! │  (TG/Discord/    Cloud)          Engine      Lattice        │
//! │   Slack/WA)                                                 │
//! │        ↓              ↓            ↓              ↓         │
//! │  User Messages   AI Response   Execution    Crypto Proof    │
//! └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Security Model
//!
//! Unlike OpenClaw which stores credentials insecurely, RopeAgent:
//! - Binds all actions to Datawallet+ identity
//! - Encrypts credentials with OES (Organic Encryption System)
//! - Requires multi-agent Testimony consensus for sensitive actions
//! - Records all actions on String Lattice for audit trail
//! - Sandboxes skill execution with capability-based permissions
//! - Rate limits API access per user
//! - Validates and sanitizes all user inputs

pub mod agents;
pub mod ai;
pub mod channels;
pub mod config;
pub mod error;
pub mod identity;
pub mod intent;
pub mod lattice_client;
pub mod memory;
pub mod runtime;
pub mod sandbox;
pub mod security;
pub mod skills;
pub mod websocket;

pub use agents::*;
pub use ai::{AIModelConfig, AIModelManager, ChatMessage, CompletionRequest, CompletionResponse};
pub use channels::*;
pub use config::RuntimeConfig;
pub use error::RuntimeError;
pub use identity::*;
pub use intent::*;
pub use lattice_client::LatticeClient;
pub use memory::EncryptedMemoryStore;
pub use runtime::RopeAgentRuntime;
pub use sandbox::{Capability, SandboxConfig, SandboxedExecutor};
pub use security::{InputValidator, RateLimiter, TieredRateLimiter, ValidationError};
pub use skills::*;
pub use websocket::{LatticeEvent, LatticeWebSocketClient, WebSocketCommand};

/// Crate version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default lattice endpoints
pub const DEFAULT_LATTICE_ENDPOINTS: &[&str] = &[
    "https://erpc.datachain.network",
    "https://erpc.rope.network",
];

/// Default WebSocket endpoint
pub const DEFAULT_WEBSOCKET_URL: &str = "wss://ws.datachain.network";
