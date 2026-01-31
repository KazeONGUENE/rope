//! AI Model Integration
//!
//! OpenClaw-style AI model integration supporting:
//! - Local LLMs (via HTTP API to llama.cpp/ollama)
//! - Cloud providers (OpenAI, Anthropic)
//! - Hybrid selection based on task complexity

mod model;
mod prompt;
mod provider;

pub use model::*;
pub use prompt::*;
pub use provider::*;

use crate::error::RuntimeError;
use crate::intent::{Entity, EntityType, Intent, IntentType};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// AI Model configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AIModelConfig {
    /// Model selection strategy
    pub strategy: ModelStrategy,

    /// Local model endpoint (e.g., ollama, llama.cpp server)
    pub local_endpoint: Option<String>,

    /// Local model name
    pub local_model: Option<String>,

    /// OpenAI API key
    pub openai_api_key: Option<String>,

    /// OpenAI model
    pub openai_model: String,

    /// Anthropic API key
    pub anthropic_api_key: Option<String>,

    /// Anthropic model
    pub anthropic_model: String,

    /// Temperature for generation
    pub temperature: f32,

    /// Max tokens for response
    pub max_tokens: u32,

    /// Minimum confidence for intent parsing
    pub min_confidence: f64,

    /// Request timeout (seconds)
    pub timeout_secs: u64,
}

impl Default for AIModelConfig {
    fn default() -> Self {
        Self {
            strategy: ModelStrategy::HybridIntelligent,
            local_endpoint: Some("http://localhost:11434".to_string()),
            local_model: Some("llama3:8b".to_string()),
            openai_api_key: None,
            openai_model: "gpt-4o-mini".to_string(),
            anthropic_api_key: None,
            anthropic_model: "claude-3-haiku-20240307".to_string(),
            temperature: 0.7,
            max_tokens: 1024,
            min_confidence: 0.7,
            timeout_secs: 30,
        }
    }
}

/// Model selection strategy
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelStrategy {
    /// Always use local model
    LocalOnly,

    /// Always use cloud model
    CloudOnly,

    /// Local first, cloud fallback
    LocalFirst,

    /// Use local for simple, cloud for complex
    HybridIntelligent,
}

/// Chat message for conversation context
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Role: system, user, assistant
    pub role: String,

    /// Message content
    pub content: String,
}

/// AI completion request
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// System prompt
    pub system_prompt: String,

    /// Conversation history
    pub messages: Vec<ChatMessage>,

    /// Temperature
    pub temperature: f32,

    /// Max tokens
    pub max_tokens: u32,
}

/// AI completion response
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CompletionResponse {
    /// Generated text
    pub content: String,

    /// Tokens used
    pub tokens_used: u32,

    /// Model that generated the response
    pub model: String,

    /// Latency in milliseconds
    pub latency_ms: u64,
}

/// Parsed intent with AI enhancement
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AIIntent {
    /// Parsed intent
    pub intent: Intent,

    /// AI reasoning (encrypted for audit)
    pub reasoning: Option<String>,

    /// Suggested response if informational
    pub suggested_response: Option<String>,

    /// Detected risks
    pub risks: Vec<String>,
}

/// Task complexity classification
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TaskComplexity {
    /// Simple queries, greetings
    Simple,

    /// Standard operations
    Medium,

    /// Complex reasoning, multi-step
    Complex,
}

impl TaskComplexity {
    /// Classify task complexity from message
    pub fn classify(message: &str) -> Self {
        let lower = message.to_lowercase();
        let word_count = message.split_whitespace().count();

        // Simple heuristics
        if word_count < 10
            && (lower.contains("help")
                || lower.contains("hello")
                || lower.contains("hi")
                || lower.contains("status")
                || lower.contains("balance"))
        {
            return Self::Simple;
        }

        // Complex indicators
        if word_count > 50
            || lower.contains("explain")
            || lower.contains("analyze")
            || lower.contains("compare")
            || lower.contains("strategy")
            || (lower.contains("if") && lower.contains("then"))
        {
            return Self::Complex;
        }

        Self::Medium
    }

    /// Should use cloud model for this complexity?
    pub fn should_use_cloud(&self, strategy: &ModelStrategy) -> bool {
        match strategy {
            ModelStrategy::CloudOnly => true,
            ModelStrategy::LocalOnly => false,
            ModelStrategy::LocalFirst => false,
            ModelStrategy::HybridIntelligent => matches!(self, Self::Complex),
        }
    }
}
