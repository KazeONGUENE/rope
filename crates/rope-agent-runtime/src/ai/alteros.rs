//! AlterOS AI Orchestrator Integration
//!
//! Intelligent AI model orchestration based on AlterOS Cognitive Operating System.
//! AlterOS decides which AI backend (Ollama, OpenAI, Anthropic) to use for each
//! agent task based on query complexity, cost optimization, and availability.
//!
//! Ported from: /Users/kazealphonseonguene/alteros
//! Original Author: Kazé A. ONGUENE - Braincities Lab
//! Integration: Datachain Foundation

use super::{
    AIProvider, AnthropicProvider, ChatMessage, CompletionRequest, CompletionResponse,
    ModelStrategy, OllamaProvider, OpenAIProvider, TaskComplexity,
};
use crate::error::RuntimeError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

// =============================================================================
// QUERY COMPLEXITY ANALYSIS (Ported from AlterOS salad_llm_client.py)
// =============================================================================

/// Response complexity levels for adaptive token management
/// Mirrors AlterOS ResponseComplexity enum
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResponseComplexity {
    /// Simple greetings, 50-100 tokens
    Greeting,
    /// Quick answers, 100-250 tokens
    Brief,
    /// Standard responses, 250-500 tokens
    Moderate,
    /// Explanations, code, 500-1000 tokens
    Detailed,
    /// Essays, analysis, 1000-2000 tokens
    Comprehensive,
    /// Long-form content, 2000-4000 tokens
    Extended,
}

impl ResponseComplexity {
    /// Get recommended max tokens for this complexity level
    pub fn max_tokens(&self) -> u32 {
        match self {
            Self::Greeting => 100,
            Self::Brief => 250,
            Self::Moderate => 500,
            Self::Detailed => 1000,
            Self::Comprehensive => 2000,
            Self::Extended => 4000,
        }
    }

    /// Get recommended temperature for this complexity
    pub fn temperature(&self) -> f32 {
        match self {
            Self::Greeting => 0.6,
            Self::Brief => 0.6,
            Self::Moderate => 0.7,
            Self::Detailed => 0.7,
            Self::Comprehensive => 0.8,
            Self::Extended => 0.8,
        }
    }
}

/// Query context flags detected during analysis
#[derive(Clone, Debug, Default)]
pub struct QueryContext {
    /// Is this a code generation request?
    pub is_code: bool,
    /// Is this a long-form content request?
    pub is_longform: bool,
    /// Is this a security-sensitive request?
    pub is_security: bool,
    /// Is this a blockchain/crypto request?
    pub is_blockchain: bool,
    /// Detected complexity level
    pub complexity: ResponseComplexity,
    /// Word count
    pub word_count: usize,
}

impl Default for ResponseComplexity {
    fn default() -> Self {
        Self::Moderate
    }
}

/// Query complexity analyzer (ported from AlterOS analyze_query_complexity)
pub struct QueryAnalyzer;

impl QueryAnalyzer {
    /// Analyze query to determine response complexity and context
    pub fn analyze(message: &str, conversation_history: Option<&[ChatMessage]>) -> QueryContext {
        let msg_lower = message.to_lowercase();
        let word_count = message.split_whitespace().count();

        // Greeting detection (from AlterOS)
        let greeting_patterns = [
            "hi", "hello", "hey", "yo", "sup", "bonjour", "salut", "ciao", "hola",
            "good morning", "good afternoon", "good evening", "good night",
            "what's up", "how are you",
        ];
        let is_greeting = greeting_patterns.iter().any(|p| msg_lower.starts_with(p)) && word_count < 10;

        // Code request detection
        let code_indicators = [
            "python", "javascript", "typescript", "java", "c++", "c#", "rust", "go",
            "solidity", "write code", "write a function", "write a script", "write a program",
            "create a function", "implement", "algorithm", "code example",
            "fibonacci", "sorting", "binary search", "recursive", "refactor",
            "debug", "fix this code", "optimize this", "code review",
            "smart contract", "contract code",
        ];
        let is_code = code_indicators.iter().any(|ind| msg_lower.contains(ind));

        // Long-form request detection
        let longform_indicators = [
            "write an essay", "write a report", "detailed analysis", "comprehensive",
            "explain in detail", "thorough explanation", "step by step guide",
            "whitepaper", "research paper", "documentation", "tutorial",
            "write a story", "write an article", "blog post", "full guide",
        ];
        let is_longform = longform_indicators.iter().any(|ind| msg_lower.contains(ind));

        // Security-sensitive detection (Datachain-specific)
        let security_indicators = [
            "private key", "seed phrase", "password", "secret", "credential",
            "authentication", "authorization", "permission", "access control",
            "vulnerability", "exploit", "audit", "cerber", "security scan",
        ];
        let is_security = security_indicators.iter().any(|ind| msg_lower.contains(ind));

        // Blockchain/crypto detection (Datachain-specific)
        let blockchain_indicators = [
            "blockchain", "transaction", "block", "consensus", "validator",
            "stake", "token", "dcfat", "dc token", "wallet", "transfer",
            "smart contract", "defi", "dao", "governance", "treasury",
            "string", "lattice", "testimony", "federation", "community",
        ];
        let is_blockchain = blockchain_indicators.iter().any(|ind| msg_lower.contains(ind));

        // Detailed response indicators
        let detailed_indicators = [
            "who are you", "what are you", "tell me about yourself",
            "describe yourself", "introduce yourself", "what makes you different",
            "explain how", "explain why", "explain what", "how does",
            "what can you do", "what are your capabilities", "compare",
        ];
        let needs_detail = detailed_indicators.iter().any(|ind| msg_lower.contains(ind));

        // Determine complexity
        let complexity = if is_greeting {
            ResponseComplexity::Greeting
        } else if is_longform {
            ResponseComplexity::Comprehensive
        } else if is_code || is_security {
            ResponseComplexity::Detailed
        } else if needs_detail {
            ResponseComplexity::Detailed
        } else if word_count < 5 {
            ResponseComplexity::Brief
        } else if word_count < 15 {
            ResponseComplexity::Moderate
        } else if word_count < 50 {
            ResponseComplexity::Detailed
        } else {
            ResponseComplexity::Comprehensive
        };

        debug!(
            "Query analysis: complexity={:?}, code={}, longform={}, security={}, blockchain={}",
            complexity, is_code, is_longform, is_security, is_blockchain
        );

        QueryContext {
            is_code,
            is_longform,
            is_security,
            is_blockchain,
            complexity,
            word_count,
        }
    }
}

// =============================================================================
// MODEL SELECTION STRATEGY (AlterOS-style intelligent routing)
// =============================================================================

/// Model routing preference based on task type
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelPreference {
    /// Use local model (Ollama) - fast, private, low cost
    Local,
    /// Use OpenAI - high quality, good for general tasks
    OpenAI,
    /// Use Anthropic - best for code, reasoning, safety
    Anthropic,
    /// Let AlterOS decide based on context
    Auto,
}

/// AlterOS model routing rules
#[derive(Clone, Debug)]
pub struct RoutingRules {
    /// Use Anthropic for security-sensitive tasks
    pub anthropic_for_security: bool,
    /// Use Anthropic for complex code generation
    pub anthropic_for_code: bool,
    /// Use local model for simple tasks
    pub local_for_simple: bool,
    /// Fallback chain when primary model unavailable
    pub fallback_chain: Vec<String>,
    /// Maximum retry attempts
    pub max_retries: u32,
    /// Cost optimization enabled
    pub cost_optimization: bool,
}

impl Default for RoutingRules {
    fn default() -> Self {
        Self {
            anthropic_for_security: true,
            anthropic_for_code: true,
            local_for_simple: true,
            fallback_chain: vec![
                "ollama".to_string(),
                "openai".to_string(),
                "anthropic".to_string(),
            ],
            max_retries: 3,
            cost_optimization: true,
        }
    }
}

// =============================================================================
// COGNITIVE INTEGRITY VERIFICATION (Ported from AlterOS CIL)
// =============================================================================

/// Integrity status for AI actions
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IntegrityStatus {
    /// All checks passed
    Verified,
    /// Minor drift detected
    DriftDetected,
    /// Constraint violation
    ConstraintViolation,
    /// Cannot verify
    Unknown,
}

/// Simple action verification (lightweight CIL port)
#[derive(Clone, Debug)]
pub struct ActionVerifier {
    /// Prohibited patterns in outputs
    prohibited_patterns: Vec<String>,
    /// Required patterns in outputs
    required_patterns: Vec<String>,
}

impl Default for ActionVerifier {
    fn default() -> Self {
        Self {
            prohibited_patterns: vec![
                // Security-sensitive patterns that should never be in output
                "private key:".to_string(),
                "seed phrase:".to_string(),
                "password:".to_string(),
            ],
            required_patterns: vec![],
        }
    }
}

impl ActionVerifier {
    /// Verify an AI response doesn't violate constraints
    pub fn verify(&self, response: &str) -> IntegrityStatus {
        let response_lower = response.to_lowercase();

        // Check prohibited patterns
        for pattern in &self.prohibited_patterns {
            if response_lower.contains(&pattern.to_lowercase()) {
                warn!("CIL: Prohibited pattern detected in response");
                return IntegrityStatus::ConstraintViolation;
            }
        }

        IntegrityStatus::Verified
    }
}

// =============================================================================
// ALTEROS ORCHESTRATOR (Main entry point)
// =============================================================================

/// Provider health status
#[derive(Clone, Debug)]
struct ProviderHealth {
    available: bool,
    last_check: Instant,
    consecutive_failures: u32,
    avg_latency_ms: u64,
}

impl Default for ProviderHealth {
    fn default() -> Self {
        Self {
            available: true,
            last_check: Instant::now(),
            consecutive_failures: 0,
            avg_latency_ms: 0,
        }
    }
}

/// AlterOS AI Orchestrator
///
/// Intelligent orchestration layer that routes AI requests to the optimal
/// provider based on task complexity, availability, cost, and performance.
pub struct AlterOSOrchestrator {
    /// Ollama (local) provider
    ollama: Option<OllamaProvider>,
    /// OpenAI provider
    openai: Option<OpenAIProvider>,
    /// Anthropic provider
    anthropic: Option<AnthropicProvider>,
    /// Routing rules
    rules: RoutingRules,
    /// Action verifier (CIL)
    verifier: ActionVerifier,
    /// Provider health tracking
    health: Arc<RwLock<HashMap<String, ProviderHealth>>>,
    /// Total requests processed
    total_requests: Arc<RwLock<u64>>,
    /// Requests by provider
    requests_by_provider: Arc<RwLock<HashMap<String, u64>>>,
}

impl AlterOSOrchestrator {
    /// Create a new AlterOS orchestrator with available providers
    pub fn new(
        ollama_endpoint: Option<&str>,
        ollama_model: Option<&str>,
        openai_api_key: Option<&str>,
        openai_model: Option<&str>,
        anthropic_api_key: Option<&str>,
        anthropic_model: Option<&str>,
    ) -> Self {
        let ollama = match (ollama_endpoint, ollama_model) {
            (Some(endpoint), Some(model)) => {
                info!("AlterOS: Initializing Ollama provider at {}", endpoint);
                Some(OllamaProvider::new(endpoint, model))
            }
            _ => None,
        };

        let openai = openai_api_key.map(|key| {
            let model = openai_model.unwrap_or("gpt-4o-mini");
            info!("AlterOS: Initializing OpenAI provider with model {}", model);
            OpenAIProvider::new(key, model)
        });

        let anthropic = anthropic_api_key.map(|key| {
            let model = anthropic_model.unwrap_or("claude-3-haiku-20240307");
            info!("AlterOS: Initializing Anthropic provider with model {}", model);
            AnthropicProvider::new(key, model)
        });

        let mut health = HashMap::new();
        if ollama.is_some() {
            health.insert("ollama".to_string(), ProviderHealth::default());
        }
        if openai.is_some() {
            health.insert("openai".to_string(), ProviderHealth::default());
        }
        if anthropic.is_some() {
            health.insert("anthropic".to_string(), ProviderHealth::default());
        }

        info!(
            "AlterOS Orchestrator initialized: ollama={}, openai={}, anthropic={}",
            ollama.is_some(),
            openai.is_some(),
            anthropic.is_some()
        );

        Self {
            ollama,
            openai,
            anthropic,
            rules: RoutingRules::default(),
            verifier: ActionVerifier::default(),
            health: Arc::new(RwLock::new(health)),
            total_requests: Arc::new(RwLock::new(0)),
            requests_by_provider: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Configure routing rules
    pub fn with_rules(mut self, rules: RoutingRules) -> Self {
        self.rules = rules;
        self
    }

    /// Get provider statistics
    pub async fn get_stats(&self) -> HashMap<String, serde_json::Value> {
        let total = *self.total_requests.read().await;
        let by_provider = self.requests_by_provider.read().await.clone();
        let health = self.health.read().await.clone();

        let mut stats = HashMap::new();
        stats.insert(
            "total_requests".to_string(),
            serde_json::json!(total),
        );
        stats.insert(
            "requests_by_provider".to_string(),
            serde_json::json!(by_provider),
        );
        stats.insert(
            "provider_health".to_string(),
            serde_json::json!(health.iter().map(|(k, v)| {
                (k.clone(), serde_json::json!({
                    "available": v.available,
                    "failures": v.consecutive_failures,
                    "avg_latency_ms": v.avg_latency_ms,
                }))
            }).collect::<HashMap<_, _>>()),
        );
        stats
    }

    /// Select the optimal provider based on context and rules
    async fn select_provider(&self, context: &QueryContext) -> Result<&str, RuntimeError> {
        let health = self.health.read().await;

        // Rule 1: Use Anthropic for security-sensitive tasks
        if self.rules.anthropic_for_security && context.is_security {
            if let Some(h) = health.get("anthropic") {
                if h.available && self.anthropic.is_some() {
                    debug!("AlterOS: Routing to Anthropic (security-sensitive task)");
                    return Ok("anthropic");
                }
            }
        }

        // Rule 2: Use Anthropic for complex code generation
        if self.rules.anthropic_for_code && context.is_code {
            if matches!(
                context.complexity,
                ResponseComplexity::Detailed
                    | ResponseComplexity::Comprehensive
                    | ResponseComplexity::Extended
            ) {
                if let Some(h) = health.get("anthropic") {
                    if h.available && self.anthropic.is_some() {
                        debug!("AlterOS: Routing to Anthropic (complex code task)");
                        return Ok("anthropic");
                    }
                }
            }
        }

        // Rule 3: Use local model for simple tasks (cost optimization)
        if self.rules.local_for_simple && self.rules.cost_optimization {
            if matches!(
                context.complexity,
                ResponseComplexity::Greeting | ResponseComplexity::Brief
            ) {
                if let Some(h) = health.get("ollama") {
                    if h.available && self.ollama.is_some() {
                        debug!("AlterOS: Routing to Ollama (simple task, cost optimization)");
                        return Ok("ollama");
                    }
                }
            }
        }

        // Rule 4: Blockchain-specific tasks - use local or OpenAI (faster)
        if context.is_blockchain && !context.is_security {
            // Try local first for blockchain queries
            if let Some(h) = health.get("ollama") {
                if h.available && self.ollama.is_some() {
                    debug!("AlterOS: Routing to Ollama (blockchain task)");
                    return Ok("ollama");
                }
            }
            // Fall back to OpenAI
            if let Some(h) = health.get("openai") {
                if h.available && self.openai.is_some() {
                    debug!("AlterOS: Routing to OpenAI (blockchain task fallback)");
                    return Ok("openai");
                }
            }
        }

        // Default fallback chain
        for provider in &self.rules.fallback_chain {
            if let Some(h) = health.get(provider) {
                if h.available {
                    match provider.as_str() {
                        "ollama" if self.ollama.is_some() => {
                            debug!("AlterOS: Using fallback provider: ollama");
                            return Ok("ollama");
                        }
                        "openai" if self.openai.is_some() => {
                            debug!("AlterOS: Using fallback provider: openai");
                            return Ok("openai");
                        }
                        "anthropic" if self.anthropic.is_some() => {
                            debug!("AlterOS: Using fallback provider: anthropic");
                            return Ok("anthropic");
                        }
                        _ => continue,
                    }
                }
            }
        }

        Err(RuntimeError::ConfigError(
            "No available AI providers".to_string(),
        ))
    }

    /// Execute a completion request through the optimal provider
    pub async fn complete(
        &self,
        mut request: CompletionRequest,
    ) -> Result<CompletionResponse, RuntimeError> {
        // Increment request counter
        {
            let mut total = self.total_requests.write().await;
            *total += 1;
        }

        // Analyze the query
        let last_message = request.messages.last().map(|m| m.content.as_str()).unwrap_or("");
        let context = QueryAnalyzer::analyze(last_message, Some(&request.messages));

        // Adjust request based on complexity
        if request.max_tokens == 0 {
            request.max_tokens = context.complexity.max_tokens();
        }
        if request.temperature == 0.0 {
            request.temperature = context.complexity.temperature();
        }

        // Select optimal provider
        let provider_name = self.select_provider(&context).await?;

        // Track provider usage
        {
            let mut by_provider = self.requests_by_provider.write().await;
            *by_provider.entry(provider_name.to_string()).or_insert(0) += 1;
        }

        // Execute with selected provider
        let start = Instant::now();
        let result = match provider_name {
            "ollama" => {
                self.ollama
                    .as_ref()
                    .unwrap()
                    .complete(request)
                    .await
            }
            "openai" => {
                self.openai
                    .as_ref()
                    .unwrap()
                    .complete(request)
                    .await
            }
            "anthropic" => {
                self.anthropic
                    .as_ref()
                    .unwrap()
                    .complete(request)
                    .await
            }
            _ => Err(RuntimeError::ExecutionError("Unknown provider".to_string())),
        };
        let latency = start.elapsed().as_millis() as u64;

        // Update health tracking
        {
            let mut health = self.health.write().await;
            if let Some(h) = health.get_mut(provider_name) {
                match &result {
                    Ok(_) => {
                        h.consecutive_failures = 0;
                        h.available = true;
                        // Exponential moving average for latency
                        h.avg_latency_ms = (h.avg_latency_ms * 3 + latency) / 4;
                    }
                    Err(_) => {
                        h.consecutive_failures += 1;
                        if h.consecutive_failures >= 3 {
                            h.available = false;
                            warn!("AlterOS: Provider {} marked unavailable after {} failures",
                                  provider_name, h.consecutive_failures);
                        }
                    }
                }
                h.last_check = Instant::now();
            }
        }

        // Verify response integrity (CIL)
        if let Ok(ref response) = result {
            let integrity = self.verifier.verify(&response.content);
            if integrity != IntegrityStatus::Verified {
                warn!(
                    "AlterOS CIL: Response integrity check failed: {:?}",
                    integrity
                );
                // In production, we might want to redact or reject the response
            }
        }

        result
    }

    /// Check if any provider is available
    pub async fn is_available(&self) -> bool {
        let health = self.health.read().await;
        health.values().any(|h| h.available)
    }

    /// Force health check on all providers
    pub async fn health_check(&self) {
        if let Some(ref ollama) = self.ollama {
            let available = ollama.is_available().await;
            let mut health = self.health.write().await;
            if let Some(h) = health.get_mut("ollama") {
                h.available = available;
                h.last_check = Instant::now();
            }
        }

        // Note: OpenAI and Anthropic availability is based on API key presence
        // and error responses, not active health checks
    }
}

#[async_trait]
impl AIProvider for AlterOSOrchestrator {
    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, RuntimeError> {
        // Delegate to the orchestrator's complete method
        AlterOSOrchestrator::complete(self, request).await
    }

    async fn is_available(&self) -> bool {
        AlterOSOrchestrator::is_available(self).await
    }

    fn name(&self) -> &str {
        "alteros"
    }
}

// =============================================================================
// ALTEROS CONFIGURATION
// =============================================================================

/// AlterOS orchestrator configuration
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AlterOSConfig {
    /// Enable AlterOS orchestration
    pub enabled: bool,

    /// Ollama configuration
    pub ollama_endpoint: Option<String>,
    pub ollama_model: Option<String>,

    /// OpenAI configuration
    pub openai_api_key: Option<String>,
    pub openai_model: Option<String>,

    /// Anthropic configuration
    pub anthropic_api_key: Option<String>,
    pub anthropic_model: Option<String>,

    /// Use Anthropic for security-sensitive tasks
    pub anthropic_for_security: bool,

    /// Use Anthropic for complex code generation
    pub anthropic_for_code: bool,

    /// Use local model for simple tasks
    pub local_for_simple: bool,

    /// Enable cost optimization
    pub cost_optimization: bool,
}

impl Default for AlterOSConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ollama_endpoint: Some("http://localhost:11434".to_string()),
            ollama_model: Some("llama3:8b".to_string()),
            openai_api_key: None,
            openai_model: Some("gpt-4o-mini".to_string()),
            anthropic_api_key: None,
            anthropic_model: Some("claude-3-haiku-20240307".to_string()),
            anthropic_for_security: true,
            anthropic_for_code: true,
            local_for_simple: true,
            cost_optimization: true,
        }
    }
}

impl AlterOSConfig {
    /// Build an AlterOS orchestrator from this configuration
    pub fn build(&self) -> AlterOSOrchestrator {
        let rules = RoutingRules {
            anthropic_for_security: self.anthropic_for_security,
            anthropic_for_code: self.anthropic_for_code,
            local_for_simple: self.local_for_simple,
            cost_optimization: self.cost_optimization,
            ..Default::default()
        };

        AlterOSOrchestrator::new(
            self.ollama_endpoint.as_deref(),
            self.ollama_model.as_deref(),
            self.openai_api_key.as_deref(),
            self.openai_model.as_deref(),
            self.anthropic_api_key.as_deref(),
            self.anthropic_model.as_deref(),
        )
        .with_rules(rules)
    }
}

// =============================================================================
// TESTS
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_analyzer_greeting() {
        let ctx = QueryAnalyzer::analyze("Hello!", None);
        assert_eq!(ctx.complexity, ResponseComplexity::Greeting);
        assert!(!ctx.is_code);
        assert!(!ctx.is_security);
    }

    #[test]
    fn test_query_analyzer_code() {
        let ctx = QueryAnalyzer::analyze("Write a Python function to sort a list", None);
        assert!(ctx.is_code);
        assert_eq!(ctx.complexity, ResponseComplexity::Detailed);
    }

    #[test]
    fn test_query_analyzer_security() {
        let ctx = QueryAnalyzer::analyze("Run a security audit on this smart contract", None);
        assert!(ctx.is_security);
        assert!(ctx.is_blockchain);
    }

    #[test]
    fn test_query_analyzer_blockchain() {
        let ctx = QueryAnalyzer::analyze("How do I transfer DC tokens?", None);
        assert!(ctx.is_blockchain);
    }

    #[test]
    fn test_query_analyzer_longform() {
        let ctx = QueryAnalyzer::analyze("Write a comprehensive report on blockchain consensus", None);
        assert!(ctx.is_longform);
        assert_eq!(ctx.complexity, ResponseComplexity::Comprehensive);
    }

    #[test]
    fn test_action_verifier() {
        let verifier = ActionVerifier::default();

        // Safe response
        assert_eq!(
            verifier.verify("The transaction was successful."),
            IntegrityStatus::Verified
        );

        // Dangerous response (would leak private key)
        assert_eq!(
            verifier.verify("Your private key: abc123..."),
            IntegrityStatus::ConstraintViolation
        );
    }

    #[test]
    fn test_complexity_tokens() {
        assert_eq!(ResponseComplexity::Greeting.max_tokens(), 100);
        assert_eq!(ResponseComplexity::Detailed.max_tokens(), 1000);
        assert_eq!(ResponseComplexity::Extended.max_tokens(), 4000);
    }
}
