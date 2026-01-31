//! AI Provider implementations
//!
//! Supports local models (Ollama, llama.cpp) and cloud (OpenAI, Anthropic)

use super::{AIModelConfig, ChatMessage, CompletionRequest, CompletionResponse, ModelStrategy};
use crate::error::RuntimeError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// AI Provider trait
#[async_trait]
pub trait AIProvider: Send + Sync {
    /// Complete a prompt
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, RuntimeError>;

    /// Check if provider is available
    async fn is_available(&self) -> bool;

    /// Get provider name
    fn name(&self) -> &str;
}

/// Ollama (local) provider
pub struct OllamaProvider {
    endpoint: String,
    model: String,
    client: reqwest::Client,
}

impl OllamaProvider {
    pub fn new(endpoint: &str, model: &str) -> Self {
        Self {
            endpoint: endpoint.to_string(),
            model: model.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct OllamaRequest {
    model: String,
    messages: Vec<OllamaMessage>,
    stream: bool,
    options: OllamaOptions,
}

#[derive(Serialize)]
struct OllamaMessage {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct OllamaOptions {
    temperature: f32,
    num_predict: u32,
}

#[derive(Deserialize)]
struct OllamaResponse {
    message: OllamaResponseMessage,
    #[serde(default)]
    eval_count: u32,
}

#[derive(Deserialize)]
struct OllamaResponseMessage {
    content: String,
}

#[async_trait]
impl AIProvider for OllamaProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, RuntimeError> {
        let start = Instant::now();

        let mut messages = vec![OllamaMessage {
            role: "system".to_string(),
            content: request.system_prompt,
        }];

        for msg in request.messages {
            messages.push(OllamaMessage {
                role: msg.role,
                content: msg.content,
            });
        }

        let ollama_request = OllamaRequest {
            model: self.model.clone(),
            messages,
            stream: false,
            options: OllamaOptions {
                temperature: request.temperature,
                num_predict: request.max_tokens,
            },
        };

        let response = self
            .client
            .post(format!("{}/api/chat", self.endpoint))
            .json(&ollama_request)
            .send()
            .await
            .map_err(|e| RuntimeError::ExecutionError(format!("Ollama request failed: {}", e)))?;

        let ollama_response: OllamaResponse = response
            .json()
            .await
            .map_err(|e| RuntimeError::ExecutionError(format!("Ollama parse failed: {}", e)))?;

        Ok(CompletionResponse {
            content: ollama_response.message.content,
            tokens_used: ollama_response.eval_count,
            model: self.model.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }

    async fn is_available(&self) -> bool {
        self.client
            .get(format!("{}/api/tags", self.endpoint))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
    }

    fn name(&self) -> &str {
        "ollama"
    }
}

/// OpenAI provider
pub struct OpenAIProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAIProvider {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct OpenAIRequest {
    model: String,
    messages: Vec<OpenAIMessage>,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Serialize)]
struct OpenAIMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAIResponse {
    choices: Vec<OpenAIChoice>,
    usage: OpenAIUsage,
}

#[derive(Deserialize)]
struct OpenAIChoice {
    message: OpenAIChoiceMessage,
}

#[derive(Deserialize)]
struct OpenAIChoiceMessage {
    content: String,
}

#[derive(Deserialize)]
struct OpenAIUsage {
    total_tokens: u32,
}

#[async_trait]
impl AIProvider for OpenAIProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, RuntimeError> {
        let start = Instant::now();

        let mut messages = vec![OpenAIMessage {
            role: "system".to_string(),
            content: request.system_prompt,
        }];

        for msg in request.messages {
            messages.push(OpenAIMessage {
                role: msg.role,
                content: msg.content,
            });
        }

        let openai_request = OpenAIRequest {
            model: self.model.clone(),
            messages,
            temperature: request.temperature,
            max_tokens: request.max_tokens,
        };

        let response = self
            .client
            .post("https://api.openai.com/v1/chat/completions")
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&openai_request)
            .send()
            .await
            .map_err(|e| RuntimeError::ExecutionError(format!("OpenAI request failed: {}", e)))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(RuntimeError::ExecutionError(format!(
                "OpenAI error: {}",
                error_text
            )));
        }

        let openai_response: OpenAIResponse = response
            .json()
            .await
            .map_err(|e| RuntimeError::ExecutionError(format!("OpenAI parse failed: {}", e)))?;

        let content = openai_response
            .choices
            .first()
            .map(|c| c.message.content.clone())
            .unwrap_or_default();

        Ok(CompletionResponse {
            content,
            tokens_used: openai_response.usage.total_tokens,
            model: self.model.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn name(&self) -> &str {
        "openai"
    }
}

/// Anthropic provider
pub struct AnthropicProvider {
    api_key: String,
    model: String,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(api_key: &str, model: &str) -> Self {
        Self {
            api_key: api_key.to_string(),
            model: model.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContent>,
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
struct AnthropicContent {
    text: String,
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

#[async_trait]
impl AIProvider for AnthropicProvider {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, RuntimeError> {
        let start = Instant::now();

        let messages: Vec<AnthropicMessage> = request
            .messages
            .into_iter()
            .map(|m| AnthropicMessage {
                role: m.role,
                content: m.content,
            })
            .collect();

        let anthropic_request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: request.max_tokens,
            system: request.system_prompt,
            messages,
        };

        let response = self
            .client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&anthropic_request)
            .send()
            .await
            .map_err(|e| RuntimeError::ExecutionError(format!("Anthropic request failed: {}", e)))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(RuntimeError::ExecutionError(format!(
                "Anthropic error: {}",
                error_text
            )));
        }

        let anthropic_response: AnthropicResponse = response
            .json()
            .await
            .map_err(|e| RuntimeError::ExecutionError(format!("Anthropic parse failed: {}", e)))?;

        let content = anthropic_response
            .content
            .first()
            .map(|c| c.text.clone())
            .unwrap_or_default();

        Ok(CompletionResponse {
            content,
            tokens_used: anthropic_response.usage.input_tokens + anthropic_response.usage.output_tokens,
            model: self.model.clone(),
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }

    async fn is_available(&self) -> bool {
        !self.api_key.is_empty()
    }

    fn name(&self) -> &str {
        "anthropic"
    }
}
