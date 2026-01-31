//! AI Model Manager
//!
//! Manages model selection and provides unified interface

use super::{
    provider::{AIProvider, AnthropicProvider, OllamaProvider, OpenAIProvider},
    AIIntent, AIModelConfig, ChatMessage, CompletionRequest, CompletionResponse, ModelStrategy,
    TaskComplexity,
};
use crate::error::RuntimeError;
use crate::intent::{Entity, EntityType, Intent, IntentType};
use std::collections::HashMap;
use std::sync::Arc;

/// AI Model Manager - handles model selection and routing
pub struct AIModelManager {
    /// Configuration
    config: AIModelConfig,

    /// Local provider (Ollama)
    local_provider: Option<Arc<dyn AIProvider>>,

    /// Cloud provider (OpenAI or Anthropic)
    cloud_provider: Option<Arc<dyn AIProvider>>,

    /// Response cache
    cache: parking_lot::RwLock<HashMap<String, CachedResponse>>,
}

#[derive(Clone)]
struct CachedResponse {
    response: CompletionResponse,
    timestamp: i64,
}

impl AIModelManager {
    /// Create new model manager
    pub fn new(config: AIModelConfig) -> Self {
        // Initialize local provider
        let local_provider: Option<Arc<dyn AIProvider>> =
            if let (Some(endpoint), Some(model)) = (&config.local_endpoint, &config.local_model) {
                Some(Arc::new(OllamaProvider::new(endpoint, model)))
            } else {
                None
            };

        // Initialize cloud provider (prefer OpenAI, fallback to Anthropic)
        let cloud_provider: Option<Arc<dyn AIProvider>> =
            if let Some(api_key) = &config.openai_api_key {
                Some(Arc::new(OpenAIProvider::new(api_key, &config.openai_model)))
            } else if let Some(api_key) = &config.anthropic_api_key {
                Some(Arc::new(AnthropicProvider::new(
                    api_key,
                    &config.anthropic_model,
                )))
            } else {
                None
            };

        Self {
            config,
            local_provider,
            cloud_provider,
            cache: parking_lot::RwLock::new(HashMap::new()),
        }
    }

    /// Complete a request using appropriate model
    pub async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, RuntimeError> {
        // Check cache
        let cache_key = self.compute_cache_key(&request);
        if let Some(cached) = self.get_cached(&cache_key) {
            return Ok(cached);
        }

        // Determine complexity
        let last_message = request.messages.last().map(|m| m.content.as_str()).unwrap_or("");
        let complexity = TaskComplexity::classify(last_message);

        // Select provider based on strategy
        let provider = self.select_provider(&complexity).await?;

        // Make request with timeout
        let response = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.timeout_secs),
            provider.complete(request),
        )
        .await
        .map_err(|_| RuntimeError::Timeout("AI model request timed out".to_string()))??;

        // Cache response
        self.cache_response(&cache_key, &response);

        Ok(response)
    }

    /// Parse intent from user message
    pub async fn parse_intent(
        &self,
        message: &str,
        context: &[ChatMessage],
    ) -> Result<AIIntent, RuntimeError> {
        let system_prompt = super::prompt::INTENT_PARSER_PROMPT.to_string();

        let mut messages = context.to_vec();
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: format!(
                "Parse the following user message and extract the intent:\n\n\"{}\"",
                message
            ),
        });

        let request = CompletionRequest {
            system_prompt,
            messages,
            temperature: 0.3, // Lower temperature for more deterministic parsing
            max_tokens: 512,
        };

        let response = self.complete(request).await?;

        // Parse the AI response into structured intent
        self.parse_intent_response(&response.content, message)
    }

    /// Generate response for informational query
    pub async fn generate_response(
        &self,
        message: &str,
        context: &[ChatMessage],
        user_info: &str,
    ) -> Result<String, RuntimeError> {
        let system_prompt = super::prompt::build_system_prompt(user_info);

        let mut messages = context.to_vec();
        messages.push(ChatMessage {
            role: "user".to_string(),
            content: message.to_string(),
        });

        let request = CompletionRequest {
            system_prompt,
            messages,
            temperature: self.config.temperature,
            max_tokens: self.config.max_tokens,
        };

        let response = self.complete(request).await?;
        Ok(response.content)
    }

    /// Select provider based on complexity and strategy
    async fn select_provider(
        &self,
        complexity: &TaskComplexity,
    ) -> Result<Arc<dyn AIProvider>, RuntimeError> {
        match &self.config.strategy {
            ModelStrategy::LocalOnly => {
                self.local_provider.clone().ok_or_else(|| {
                    RuntimeError::ConfigError("Local model not configured".to_string())
                })
            }
            ModelStrategy::CloudOnly => {
                self.cloud_provider.clone().ok_or_else(|| {
                    RuntimeError::ConfigError("Cloud model not configured".to_string())
                })
            }
            ModelStrategy::LocalFirst => {
                if let Some(local) = &self.local_provider {
                    if local.is_available().await {
                        return Ok(local.clone());
                    }
                }
                self.cloud_provider.clone().ok_or_else(|| {
                    RuntimeError::ConfigError("No AI provider available".to_string())
                })
            }
            ModelStrategy::HybridIntelligent => {
                if complexity.should_use_cloud(&self.config.strategy) {
                    if let Some(cloud) = &self.cloud_provider {
                        if cloud.is_available().await {
                            return Ok(cloud.clone());
                        }
                    }
                }
                if let Some(local) = &self.local_provider {
                    if local.is_available().await {
                        return Ok(local.clone());
                    }
                }
                self.cloud_provider.clone().ok_or_else(|| {
                    RuntimeError::ConfigError("No AI provider available".to_string())
                })
            }
        }
    }

    /// Parse AI response into structured intent
    fn parse_intent_response(
        &self,
        response: &str,
        original_message: &str,
    ) -> Result<AIIntent, RuntimeError> {
        // Try to parse JSON response
        if let Ok(parsed) = serde_json::from_str::<IntentParseResult>(response) {
            let intent_type = self.map_intent_type(&parsed.intent_type, &parsed.parameters);

            let entities: HashMap<String, Entity> = parsed
                .entities
                .into_iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        Entity {
                            entity_type: self.map_entity_type(&k),
                            value: v,
                            start: 0,
                            end: 0,
                            confidence: parsed.confidence,
                        },
                    )
                })
                .collect();

            let intent = Intent {
                intent_type,
                confidence: parsed.confidence,
                entities,
                raw_text: original_message.to_string(),
                parsed_at: chrono::Utc::now().timestamp(),
            };

            return Ok(AIIntent {
                intent,
                reasoning: Some(parsed.reasoning),
                suggested_response: parsed.suggested_response,
                risks: parsed.risks,
            });
        }

        // Fallback to basic intent parsing
        let intent = crate::intent::IntentParser::new().parse(original_message);

        Ok(AIIntent {
            intent,
            reasoning: None,
            suggested_response: None,
            risks: Vec::new(),
        })
    }

    fn map_intent_type(
        &self,
        intent_type: &str,
        params: &HashMap<String, String>,
    ) -> IntentType {
        match intent_type.to_lowercase().as_str() {
            "transfer" => IntentType::Transfer {
                asset: params.get("asset").cloned().unwrap_or_else(|| "FAT".to_string()),
                amount: params
                    .get("amount")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0),
                recipient: params.get("recipient").cloned().unwrap_or_default(),
            },
            "swap" => IntentType::Swap {
                from_asset: params
                    .get("from_asset")
                    .cloned()
                    .unwrap_or_else(|| "FAT".to_string()),
                to_asset: params
                    .get("to_asset")
                    .cloned()
                    .unwrap_or_else(|| "USDT".to_string()),
                amount: params
                    .get("amount")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0),
            },
            "stake" => IntentType::Stake {
                amount: params
                    .get("amount")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0.0),
                validator: params.get("validator").cloned(),
            },
            "status" | "balance" => IntentType::Status {
                resource: params
                    .get("resource")
                    .cloned()
                    .unwrap_or_else(|| "balance".to_string()),
            },
            "help" => IntentType::Help,
            "reminder" => IntentType::SetReminder {
                time: params
                    .get("time")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(chrono::Utc::now().timestamp() + 3600),
                message: params.get("message").cloned().unwrap_or_default(),
            },
            _ => IntentType::Query {
                topic: params
                    .get("topic")
                    .cloned()
                    .unwrap_or_else(|| intent_type.to_string()),
            },
        }
    }

    fn map_entity_type(&self, entity_name: &str) -> EntityType {
        match entity_name.to_lowercase().as_str() {
            "amount" => EntityType::Amount,
            "asset" | "from_asset" | "to_asset" => EntityType::Asset,
            "address" | "recipient" => EntityType::Address,
            "contract" => EntityType::Contract,
            "time" | "datetime" => EntityType::DateTime,
            "duration" => EntityType::Duration,
            "person" | "contact" => EntityType::Person,
            "channel" => EntityType::Channel,
            "skill" => EntityType::Skill,
            _ => EntityType::Custom(entity_name.to_string()),
        }
    }

    fn compute_cache_key(&self, request: &CompletionRequest) -> String {
        let mut key_data = request.system_prompt.clone();
        for msg in &request.messages {
            key_data.push_str(&msg.role);
            key_data.push_str(&msg.content);
        }
        blake3::hash(key_data.as_bytes()).to_hex().to_string()
    }

    fn get_cached(&self, key: &str) -> Option<CompletionResponse> {
        let cache = self.cache.read();
        if let Some(cached) = cache.get(key) {
            // Cache valid for 5 minutes
            if chrono::Utc::now().timestamp() - cached.timestamp < 300 {
                return Some(cached.response.clone());
            }
        }
        None
    }

    fn cache_response(&self, key: &str, response: &CompletionResponse) {
        let mut cache = self.cache.write();
        cache.insert(
            key.to_string(),
            CachedResponse {
                response: response.clone(),
                timestamp: chrono::Utc::now().timestamp(),
            },
        );

        // Limit cache size
        if cache.len() > 1000 {
            let oldest_key = cache
                .iter()
                .min_by_key(|(_, v)| v.timestamp)
                .map(|(k, _)| k.clone());
            if let Some(key) = oldest_key {
                cache.remove(&key);
            }
        }
    }
}

/// JSON structure for intent parsing response
#[derive(serde::Deserialize)]
struct IntentParseResult {
    intent_type: String,
    confidence: f64,
    parameters: HashMap<String, String>,
    entities: HashMap<String, String>,
    reasoning: String,
    suggested_response: Option<String>,
    #[serde(default)]
    risks: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_complexity() {
        assert_eq!(TaskComplexity::classify("help"), TaskComplexity::Simple);
        assert_eq!(TaskComplexity::classify("transfer 100 FAT"), TaskComplexity::Medium);
        assert_eq!(
            TaskComplexity::classify("explain the difference between proof of work and proof of stake and compare their environmental impact"),
            TaskComplexity::Complex
        );
    }

    #[test]
    fn test_cache_key() {
        let config = AIModelConfig::default();
        let manager = AIModelManager::new(config);

        let request = CompletionRequest {
            system_prompt: "test".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: "hello".to_string(),
            }],
            temperature: 0.7,
            max_tokens: 100,
        };

        let key1 = manager.compute_cache_key(&request);
        let key2 = manager.compute_cache_key(&request);

        assert_eq!(key1, key2);
    }
}
